use std::fs;

use peri_agent::prelude::*;
use peri_middlewares::middleware::{FilesystemMiddleware, TerminalMiddleware};
use tempfile::TempDir;

// ── 辅助：创建临时目录并写入测试文件 ────────────────────────────────────────

fn setup_temp_dir() -> TempDir {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("hello.txt"), "Hello, world!").unwrap();
    dir
}

// ── FilesystemMiddleware 自动注册测试 ─────────────────────────────────────────

/// 验证 FilesystemMiddleware 通过 add_middleware 自动提供 Read 工具
#[tokio::test]
#[allow(non_snake_case)]
async fn test_filesystem_middleware_auto_registers_Read() {
    let dir = setup_temp_dir();
    let cwd = dir.path().to_str().unwrap().to_string();

    let llm = MockLLM::tool_then_answer(
        "Read",
        serde_json::json!({ "file_path": "hello.txt" }),
        "The file contains: Hello, world!",
    );

    // 仅通过 add_middleware，不手动 register_tool
    let agent = ReActAgent::new(llm).add_middleware(Box::new(FilesystemMiddleware::new()));
    let mut state = AgentState::new(&cwd);

    let output = agent
        .execute(AgentInput::text("read hello.txt"), &mut state, None)
        .await
        .unwrap();

    assert_eq!(output.tool_calls.len(), 1);
    assert_eq!(output.tool_calls[0].0.name, "Read");
    assert!(!output.tool_calls[0].1.is_error, "Read 不应报错");
    assert!(
        output.tool_calls[0].1.output.contains("Hello, world!"),
        "工具输出应包含文件内容，实际输出: {}",
        output.tool_calls[0].1.output
    );
}

/// 验证 FilesystemMiddleware 提供所有预期的文件系统工具
#[tokio::test]
async fn test_filesystem_middleware_provides_all_tools() {
    let dir = setup_temp_dir();
    let cwd = dir.path().to_str().unwrap();

    let tools = FilesystemMiddleware::build_tools(cwd);
    let tool_names: Vec<&str> = tools.iter().map(|t| t.name()).collect();

    for expected in FilesystemMiddleware::tool_names() {
        assert!(
            tool_names.contains(&expected),
            "FilesystemMiddleware 应提供 '{expected}' 工具，实际: {tool_names:?}"
        );
    }
}

/// 验证 TerminalMiddleware 通过 add_middleware 自动提供 bash 工具
#[tokio::test]
async fn test_terminal_middleware_auto_registers_bash() {
    let dir = setup_temp_dir();
    let cwd = dir.path().to_str().unwrap().to_string();

    let llm = MockLLM::tool_then_answer(
        "Bash",
        serde_json::json!({ "command": "echo hello" }),
        "Got output: hello",
    );

    let agent = ReActAgent::new(llm).add_middleware(Box::new(TerminalMiddleware::new()));
    let mut state = AgentState::new(&cwd);

    let output = agent
        .execute(AgentInput::text("run echo"), &mut state, None)
        .await
        .unwrap();

    assert_eq!(output.tool_calls.len(), 1);
    assert_eq!(output.tool_calls[0].0.name, "Bash");
    assert!(!output.tool_calls[0].1.is_error, "Bash 工具不应报错");
    assert!(
        output.tool_calls[0].1.output.contains("hello"),
        "bash 输出应包含 'hello'，实际: {}",
        output.tool_calls[0].1.output
    );
}

/// 验证手动 register_tool 的工具优先于 FilesystemMiddleware 提供的同名工具
#[tokio::test]
async fn test_manual_tool_overrides_filesystem_middleware() {
    use async_trait::async_trait;

    struct MockReadFile;

    #[async_trait]
    impl BaseTool for MockReadFile {
        fn name(&self) -> &str {
            "Read"
        }
        fn description(&self) -> &str {
            "Mock Read"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({ "type": "object", "properties": { "file_path": { "type": "string" } } })
        }
        async fn invoke(
            &self,
            _: serde_json::Value,
            _ctx: peri_agent::tools::ToolContext<'_>,
        ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
            Ok("mocked content".to_string())
        }
    }

    let dir = setup_temp_dir();
    let cwd = dir.path().to_str().unwrap().to_string();

    let llm = MockLLM::tool_then_answer(
        "Read",
        serde_json::json!({ "file_path": "hello.txt" }),
        "done",
    );

    // FilesystemMiddleware 和 MockReadFile 均提供 Read，手动注册的应优先
    let agent = ReActAgent::new(llm)
        .add_middleware(Box::new(FilesystemMiddleware::new()))
        .register_tool(Box::new(MockReadFile));
    let mut state = AgentState::new(&cwd);

    let output = agent
        .execute(AgentInput::text("read file"), &mut state, None)
        .await
        .unwrap();

    assert_eq!(output.tool_calls.len(), 1);
    assert_eq!(
        output.tool_calls[0].1.output, "mocked content",
        "手动注册的 MockReadFile 应覆盖 FilesystemMiddleware 的 Read"
    );
}
