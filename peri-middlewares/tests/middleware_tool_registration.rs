use std::fs;

use peri_middlewares::middleware::{FilesystemMiddleware, TerminalMiddleware};
use tempfile::TempDir;

// ── 辅助：创建临时目录并写入测试文件 ────────────────────────────────────────

fn setup_temp_dir() -> TempDir {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("hello.txt"), "Hello, world!").unwrap();
    dir
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

/// 验证 TerminalMiddleware 提供所有预期的终端工具
#[tokio::test]
async fn test_terminal_middleware_provides_all_tools() {
    let cwd = "/tmp";
    let tools = TerminalMiddleware::build_tools(cwd);
    let tool_names: Vec<&str> = tools.iter().map(|t| t.name()).collect();

    assert!(
        tool_names.contains(&"Bash"),
        "TerminalMiddleware 应提供 'Bash' 工具，实际: {tool_names:?}"
    );
}
