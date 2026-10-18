use super::*;
#[cfg(unix)]
use peri_agent::tools::{BaseTool, ToolContext};
#[cfg(unix)]
use serde_json::json;

#[cfg(unix)]
fn make_symlink_workspace() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let workspace = root.join("workspace");
    let other = root.join("other");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::create_dir_all(other.join("child")).unwrap();
    std::os::unix::fs::symlink(other.join("child"), workspace.join("link")).unwrap();
    std::fs::write(workspace.join("a.txt"), "sibling\n").unwrap();
    std::fs::write(other.join("a.txt"), "target\n").unwrap();
    (directory, workspace, other)
}

/// [回归测试] 必须先跟随链接再处理 ..，不能先词法折叠成兄弟文件。
#[cfg(unix)]
#[test]
fn test_resolve_path_resolves_symlink_before_dotdot() {
    let (_directory, workspace, other) = make_symlink_workspace();
    let resolved = resolve_path(workspace.to_str().unwrap(), "link/../a.txt");
    assert_eq!(resolved, other.join("a.txt").canonicalize().unwrap());
    assert_eq!(std::fs::read_to_string(resolved).unwrap(), "target\n");
}

#[cfg(unix)]
#[test]
fn test_resolve_path_new_file_under_symlink_parent() {
    let (_directory, workspace, other) = make_symlink_workspace();
    assert_eq!(
        resolve_path(workspace.to_str().unwrap(), "link/../new.txt"),
        other.join("new.txt")
    );
}

/// [回归测试] 缺失多层目录时也必须解析已有祖先中的链接。
#[cfg(unix)]
#[test]
fn test_resolve_path_nested_missing_dirs_under_symlink_parent() {
    let (_directory, workspace, other) = make_symlink_workspace();
    assert_eq!(
        resolve_path(
            workspace.to_str().unwrap(),
            "link/../missing/nested/new.txt"
        ),
        other.join("missing/nested/new.txt")
    );
}

/// [回归测试] Read 与 Edit 曾因路径解析顺序不同命中不同文件。
#[cfg(unix)]
#[tokio::test]
async fn test_read_and_edit_target_same_symlink_dotdot_path() {
    let (_directory, workspace, other) = make_symlink_workspace();
    let cwd = workspace.to_str().unwrap();
    let read = ReadFileTool::new(cwd)
        .invoke(
            json!({"file_path": "link/../a.txt"}),
            ToolContext::new(&[], cwd),
        )
        .await
        .unwrap();
    assert!(read.contains("1\ttarget"), "应读到链接的真实目标：{read}");
    EditFileTool::new(cwd)
        .invoke(
            json!({"file_path": "link/../a.txt", "old_string": "target", "new_string": "edited"}),
            ToolContext::new(&[], cwd),
        )
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(other.join("a.txt")).unwrap(),
        "edited\n"
    );
    assert_eq!(
        std::fs::read_to_string(workspace.join("a.txt")).unwrap(),
        "sibling\n"
    );
}

/// [回归测试] 原子替换后检查目标内容，而不是已经改变的 inode。
#[cfg(unix)]
#[tokio::test]
async fn test_read_and_write_target_same_symlink_dotdot_path() {
    let (_directory, workspace, other) = make_symlink_workspace();
    let cwd = workspace.to_str().unwrap();
    let read = ReadFileTool::new(cwd)
        .invoke(
            json!({"file_path": "link/../a.txt"}),
            ToolContext::new(&[], cwd),
        )
        .await
        .unwrap();
    assert!(read.contains("1\ttarget"), "应读到链接的真实目标：{read}");
    WriteFileTool::with_draft(cwd, false)
        .invoke(
            json!({"file_path": "link/../a.txt", "content": "written\n"}),
            ToolContext::new(&[], cwd),
        )
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(other.join("a.txt")).unwrap(),
        "written\n"
    );
    assert_eq!(
        std::fs::read_to_string(workspace.join("a.txt")).unwrap(),
        "sibling\n"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn test_write_creates_new_file_at_real_symlink_target() {
    let (_directory, workspace, other) = make_symlink_workspace();
    let cwd = workspace.to_str().unwrap();
    WriteFileTool::with_draft(cwd, false)
        .invoke(
            json!({"file_path": "link/../new.txt", "content": "created\n"}),
            ToolContext::new(&[], cwd),
        )
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(other.join("new.txt")).unwrap(),
        "created\n"
    );
    assert!(!workspace.join("new.txt").exists());
    let read = ReadFileTool::new(cwd)
        .invoke(
            json!({"file_path": "link/../new.txt"}),
            ToolContext::new(&[], cwd),
        )
        .await
        .unwrap();
    assert!(
        read.contains("1\tcreated"),
        "新文件应能通过相同路径读回：{read}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn test_write_nested_new_file_through_symlink_dotdot() {
    let (_directory, workspace, other) = make_symlink_workspace();
    let cwd = workspace.to_str().unwrap();
    WriteFileTool::with_draft(cwd, false)
        .invoke(
            json!({"file_path": "link/../sub/nested/new.txt", "content": "created\n"}),
            ToolContext::new(&[], cwd),
        )
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(other.join("sub/nested/new.txt")).unwrap(),
        "created\n"
    );
    assert!(!workspace.join("sub").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn test_parent_directory_symlink_without_dotdot_is_stable() {
    let (_directory, workspace, other) = make_symlink_workspace();
    std::os::unix::fs::symlink(&other, workspace.join("linkdir")).unwrap();
    let cwd = workspace.to_str().unwrap();
    WriteFileTool::with_draft(cwd, false)
        .invoke(
            json!({"file_path": "linkdir/a.txt", "content": "written\n"}),
            ToolContext::new(&[], cwd),
        )
        .await
        .unwrap();
    assert_eq!(
        workspace.join("linkdir/a.txt").canonicalize().unwrap(),
        other.join("a.txt")
    );
    assert_eq!(
        std::fs::read_to_string(other.join("a.txt")).unwrap(),
        "written\n"
    );
    assert_eq!(
        std::fs::read_to_string(workspace.join("a.txt")).unwrap(),
        "sibling\n"
    );
}

#[test]
fn test_resolve_path_relative() {
    let directory = tempfile::tempdir().unwrap();
    let result = resolve_path(directory.path().to_str().unwrap(), "new.txt");
    assert_eq!(
        result,
        directory.path().canonicalize().unwrap().join("new.txt")
    );
}

#[test]
fn test_resolve_path_absolute() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("absolute.txt");
    std::fs::write(&file, "preserved content").unwrap();
    let result = resolve_path(directory.path().to_str().unwrap(), file.to_str().unwrap());
    assert_eq!(result, file.canonicalize().unwrap());
    assert_eq!(
        std::fs::read_to_string(result).unwrap(),
        "preserved content"
    );
}

#[test]
fn test_resolve_path_traversal_canonicalized() {
    let directory = tempfile::tempdir().unwrap();
    let child = directory.path().join("child");
    std::fs::create_dir(&child).unwrap();
    let result = resolve_path(child.to_str().unwrap(), "../new.txt");
    assert_eq!(
        result,
        directory.path().canonicalize().unwrap().join("new.txt")
    );
}
