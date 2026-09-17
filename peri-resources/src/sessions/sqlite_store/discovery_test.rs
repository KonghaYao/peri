use super::*;

#[tokio::test]
async fn test_worktree_discovery_output_is_bounded_before_eof() {
    // This source never reaches EOF; reading one byte beyond the budget must stop it.
    let error = bounded_output(tokio::io::repeat(b'x')).await.unwrap_err();
    assert!(
        matches!(error.downcast_ref::<WorkspaceError>(), Some(WorkspaceError::DiscoveryError(message)) if message.contains("bound"))
    );
}

#[tokio::test]
async fn test_worktree_discovery_finite_output_is_preserved() {
    let bytes = b"worktree /tmp/path with spaces\0HEAD hash\0";
    assert_eq!(bounded_output(&bytes[..]).await.unwrap(), bytes);
}

/// [回归测试] 未安装 Git 时仍可建立并复核目录工作区，不把父目录猜作项目根。
#[tokio::test]
async fn test_worktree_missing_git_uses_exact_directory() {
    let directory = tempfile::tempdir().unwrap();
    let nested = directory.path().join("nested space");
    tokio::fs::create_dir(&nested).await.unwrap();
    let missing = directory.path().join("missing-git");
    let (cwd, discovered) = discover_with_git(&nested, missing.as_os_str())
        .await
        .unwrap();
    assert_eq!(cwd, tokio::fs::canonicalize(&nested).await.unwrap());
    assert_eq!(discovered.root, cwd);
    assert_eq!(discovered.project_locator(), cwd);
    assert_eq!(discovered.common_dir, None);
    assert_eq!(discovered.private_dir, None);
    discovered
        .revalidate_with_git(&nested, missing.as_os_str())
        .await
        .unwrap();
    // 安装 Git 后，仍是普通目录时身份不变。
    discovered.revalidate(&nested).await.unwrap();
}

#[tokio::test]
async fn test_worktree_missing_git_does_not_hide_unavailable_directory() {
    let directory = tempfile::tempdir().unwrap();
    let error = discover_with_git(
        &directory.path().join("missing-directory"),
        directory.path().join("missing-git").as_os_str(),
    )
    .await
    .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<WorkspaceError>(),
        Some(WorkspaceError::Unavailable)
    ));
}

/// [回归测试] Git 安装状态改变不能把已绑定的仓库会话改绑为目录，反向同样禁止。
#[tokio::test]
async fn test_worktree_git_availability_change_preserves_binding_boundary() {
    let directory = tempfile::tempdir().unwrap();
    let output = Command::new("git")
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", directory.path())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["init", "-q"])
        .arg(directory.path())
        .output()
        .await
        .unwrap();
    assert!(output.status.success(), "Git fixture 创建失败: {output:?}");
    let (_, repository) = discover(directory.path()).await.unwrap();
    let missing = directory.path().join("missing-git");
    let error = repository
        .revalidate_with_git(directory.path(), missing.as_os_str())
        .await
        .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<WorkspaceError>(),
        Some(WorkspaceError::NeedsRelink)
    ));
    let (_, plain) = discover_with_git(directory.path(), missing.as_os_str())
        .await
        .unwrap();
    let error = plain.revalidate(directory.path()).await.unwrap_err();
    assert!(matches!(
        error.downcast_ref::<WorkspaceError>(),
        Some(WorkspaceError::NeedsRelink)
    ));
    // Git 恢复可用后，原仓库绑定仍可复核，不受目录模式影响。
    repository.revalidate(directory.path()).await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn test_worktree_git_permission_denied_is_not_directory_mode() {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().unwrap();
    let program = directory.path().join("git");
    std::fs::write(&program, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o600)).unwrap();
    let error = discover_with_git(directory.path(), program.as_os_str())
        .await
        .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<WorkspaceError>(),
        Some(WorkspaceError::DiscoveryError(message)) if message == "Git could not be executed"
    ));
}

#[cfg(unix)]
#[tokio::test]
async fn test_worktree_git_rejection_is_not_directory_mode() {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().unwrap();
    let program = directory.path().join("git");
    std::fs::write(
        &program,
        "#!/bin/sh\nprintf 'fatal: detected dubious ownership' >&2\nexit 128\n",
    )
    .unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    let error = discover_with_git(directory.path(), program.as_os_str())
        .await
        .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<WorkspaceError>(),
        Some(WorkspaceError::DiscoveryError(message)) if message == "Git rejected repository discovery"
    ));
}

#[tokio::test]
async fn test_worktree_git_missing_mid_discovery_is_not_directory_mode() {
    let directory = tempfile::tempdir().unwrap();
    let error = git_path(
        directory.path().join("missing-git").as_os_str(),
        directory.path(),
        &["rev-parse", "--absolute-git-dir"],
    )
    .await
    .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<WorkspaceError>(),
        Some(WorkspaceError::DiscoveryError(message)) if message == "Git became unavailable during discovery"
    ));
}
