//! 搜索范围与截断计数的用户可见契约。
use std::ffi::OsString;

use peri_agent::tools::{BaseTool, ToolContext};
use serde_json::json;

use super::super::glob::GlobFilesTool;
use super::GrepTool;

struct IsolatedEnvironment {
    _lock: crate::process_env::EnvLockFile,
    previous: Vec<(&'static str, Option<OsString>)>,
    _home: tempfile::TempDir,
}

impl IsolatedEnvironment {
    fn new() -> Self {
        let lock = crate::process_env::lock().unwrap();
        let home = tempfile::tempdir().unwrap();
        let mut previous = Vec::new();
        for key in [
            "HOME",
            "XDG_CONFIG_HOME",
            "GIT_CONFIG_GLOBAL",
            "GIT_CONFIG_SYSTEM",
        ] {
            previous.push((key, std::env::var_os(key)));
            let value = if key.starts_with("GIT_CONFIG_") {
                home.path().join("empty-git-config")
            } else {
                home.path().to_path_buf()
            };
            std::env::set_var(key, value);
        }
        Self {
            _lock: lock,
            previous,
            _home: home,
        }
    }
}

impl Drop for IsolatedEnvironment {
    fn drop(&mut self) {
        for (key, value) in self.previous.drain(..) {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
}

fn collected_output(result: &str) -> String {
    let path = result
        .split("[Full output saved to ")
        .nth(1)
        .unwrap()
        .split(" — ")
        .next()
        .unwrap();
    let content = std::fs::read_to_string(path).unwrap();
    std::fs::remove_file(path).unwrap();
    content
}

/// [回归测试] 超过结果上限即早停，1001 是已收集量而不是树中总数。
#[tokio::test]
async fn glob_result_limit_reports_collected_count() {
    let _environment = IsolatedEnvironment::new();
    let root = tempfile::tempdir().unwrap();
    for index in 0..1200 {
        std::fs::write(root.path().join(format!("item-{index:04}.txt")), "").unwrap();
    }
    let result = GlobFilesTool::new(root.path().to_str().unwrap())
        .invoke(json!({"pattern":"*.txt"}), ToolContext::new(&[], "."))
        .await
        .unwrap();
    assert!(
        result.contains("1001 files collected; total unknown"),
        "{result}"
    );
    assert!(!result.contains("1001 files total"));
    assert_eq!(collected_output(&result).lines().count(), 1001);
}

#[tokio::test]
async fn glob_byte_limit_after_complete_walk_reports_total() {
    let _environment = IsolatedEnvironment::new();
    let root = tempfile::tempdir().unwrap();
    for index in 0..220 {
        std::fs::write(
            root.path()
                .join(format!("{}-{index:04}.txt", "x".repeat(100))),
            "",
        )
        .unwrap();
    }
    let result = GlobFilesTool::new(root.path().to_str().unwrap())
        .invoke(json!({"pattern":"*.txt"}), ToolContext::new(&[], "."))
        .await
        .unwrap();
    assert!(result.contains("220 files total"), "{result}");
    assert!(!result.contains("total unknown"));
    assert_eq!(collected_output(&result).lines().count(), 220);
}

/// [回归测试] 字节预算停止扫描时，不得将输出缓冲区行数称为全部匹配数。
#[tokio::test]
async fn grep_byte_limit_reports_collected_output() {
    let _environment = IsolatedEnvironment::new();
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("a.txt"),
        format!("NEEDLE {}\n", "x".repeat(220)).repeat(300),
    )
    .unwrap();
    let result = GrepTool::new(root.path().to_str().unwrap())
        .invoke(
            json!({"pattern":"NEEDLE", "head_limit":0}),
            ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    let collected = collected_output(&result);
    assert!(collected.lines().count() < 300);
    assert!(
        result.contains(&format!(
            "{} output lines collected; total unknown",
            collected.lines().count()
        )),
        "{result}"
    );
    assert!(result.contains(&format!("{} collected bytes", collected.len())));
    assert!(!result.contains("lines total"));
}

#[tokio::test]
async fn grep_line_limit_reports_collected_output() {
    let _environment = IsolatedEnvironment::new();
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("a.txt"), "NEEDLE\n".repeat(30)).unwrap();
    let result = GrepTool::new(root.path().to_str().unwrap())
        .invoke(
            json!({"pattern":"NEEDLE", "head_limit":3}),
            ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    let collected = collected_output(&result);
    assert!(
        result.contains(&format!(
            "{} output lines collected; total unknown",
            collected.lines().count()
        )),
        "{result}"
    );
    assert!(collected.lines().count() < 30);
}

#[tokio::test]
async fn grep_byte_render_limit_after_complete_scan_reports_total() {
    let _environment = IsolatedEnvironment::new();
    let root = tempfile::tempdir().unwrap();
    // 每行格式化后恰100字节，预算不含分隔换行；200行完整枚举，join后20199字节。
    std::fs::write(
        root.path().join("a"),
        format!("{}\n", "x".repeat(97)).repeat(200),
    )
    .unwrap();
    let result = GrepTool::new(root.path().to_str().unwrap())
        .invoke(
            json!({"pattern":"x", "head_limit":0, "show_line_numbers":false}),
            ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(result.contains("200 output lines total"), "{result}");
    assert!(!result.contains("total unknown"));
    let collected = collected_output(&result);
    assert_eq!(collected.lines().count(), 200);
    assert_eq!(collected.len(), 20_199);
}

async fn assert_search_policy(git_repository: bool) {
    let _environment = IsolatedEnvironment::new();
    let root = tempfile::tempdir().unwrap();
    if git_repository {
        std::fs::create_dir(root.path().join(".git")).unwrap();
    }
    std::fs::create_dir(root.path().join(".hidden")).unwrap();
    for path in [
        "visible.txt",
        "gitignored.txt",
        "ignored.txt",
        ".hidden/hidden.txt",
    ] {
        std::fs::write(root.path().join(path), "NEEDLE").unwrap();
    }
    std::fs::write(root.path().join(".gitignore"), "gitignored.txt\n").unwrap();
    std::fs::write(root.path().join(".ignore"), "ignored.txt\n").unwrap();
    let grep = GrepTool::new(root.path().to_str().unwrap());
    let result = grep
        .invoke(json!({"pattern":"NEEDLE"}), ToolContext::new(&[], "."))
        .await
        .unwrap();
    assert!(result.contains("visible.txt"));
    assert!(!result.contains("hidden.txt"));
    assert!(!result.lines().any(|line| line.starts_with("ignored.txt:")));
    assert_eq!(
        result.contains("gitignored.txt"),
        !git_repository,
        "{result}"
    );
    let glob = GlobFilesTool::new(root.path().to_str().unwrap())
        .invoke(json!({"pattern":"*.txt"}), ToolContext::new(&[], "."))
        .await
        .unwrap();
    for path in [
        "visible.txt",
        "gitignored.txt",
        "ignored.txt",
        ".hidden/hidden.txt",
    ] {
        assert!(glob.contains(path), "Glob 应包含 {path}: {glob}");
        let explicit = grep
            .invoke(
                json!({"pattern":"NEEDLE", "path":root.path().join(path)}),
                ToolContext::new(&[], "."),
            )
            .await
            .unwrap();
        assert!(
            explicit.contains("NEEDLE"),
            "显式文件路径应可搜索 {path}: {explicit}"
        );
    }
}

#[tokio::test]
async fn ignore_file_applies_outside_git_but_gitignore_does_not() {
    assert_search_policy(false).await;
}

#[tokio::test]
async fn grep_and_glob_keep_their_distinct_repository_search_policies() {
    assert_search_policy(true).await;
}

#[cfg(unix)]
#[tokio::test]
async fn glob_explicit_symlink_root_is_resolved() {
    let _environment = IsolatedEnvironment::new();
    let root = tempfile::tempdir().unwrap();
    let target = root.path().join("real");
    std::fs::create_dir(&target).unwrap();
    std::fs::write(target.join("a.rs"), "").unwrap();
    let link = root.path().join("linked");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let result = GlobFilesTool::new(root.path().to_str().unwrap())
        .invoke(
            json!({"pattern":"*.rs", "path":link}),
            ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert_eq!(
        std::path::Path::new(&result),
        target.canonicalize().unwrap().join("a.rs")
    );
}
