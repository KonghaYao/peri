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
    let (cwd, observed) = observe_with_git(&nested, missing.as_os_str())
        .await
        .unwrap();
    // Git 未回答：目录模式观测不完整，登记层不得据此改写已登记的 Git 布局。
    assert!(!observed.git_answered);
    let discovered = observed.discovery;
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
    let error = observe_with_git(
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
    let (_, repository) = observe(directory.path()).await.unwrap();
    let missing = directory.path().join("missing-git");
    let error = repository
        .discovery
        .revalidate_with_git(directory.path(), missing.as_os_str())
        .await
        .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<WorkspaceError>(),
        Some(WorkspaceError::NeedsRelink)
    ));
    let (_, plain) = observe_with_git(directory.path(), missing.as_os_str())
        .await
        .unwrap();
    assert!(!plain.git_answered);
    let error = plain
        .discovery
        .revalidate(directory.path())
        .await
        .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<WorkspaceError>(),
        Some(WorkspaceError::NeedsRelink)
    ));
    // Git 恢复可用后，原仓库绑定仍可复核，不受目录模式影响。
    repository
        .discovery
        .revalidate(directory.path())
        .await
        .unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn test_worktree_git_permission_denied_is_not_directory_mode() {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().unwrap();
    let program = directory.path().join("git");
    std::fs::write(&program, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o600)).unwrap();
    let error = observe_with_git(directory.path(), program.as_os_str())
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
    let error = observe_with_git(directory.path(), program.as_os_str())
        .await
        .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<WorkspaceError>(),
        Some(WorkspaceError::DiscoveryError(message)) if message == "Git rejected repository discovery"
    ));
}

/// [回归测试] 提交前的轻量复核只接受关键文件对象未变的观测。
///
/// 这是写事务内唯一允许执行的重验（设计 §3.2：探测在事务外，提交前复核关键文件
/// 对象与关联关系），因此它必须在不启动外部进程的前提下仍然拒绝失效观测。
#[tokio::test]
async fn test_worktree_key_object_reassertion_rejects_changed_objects() {
    let directory = tempfile::tempdir().unwrap();
    let repository = directory.path().join("repository");
    tokio::fs::create_dir(&repository).await.unwrap();
    let output = Command::new("git")
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", directory.path())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["init", "-q"])
        .arg(&repository)
        .output()
        .await
        .unwrap();
    assert!(output.status.success(), "Git fixture 创建失败: {output:?}");
    let (cwd, observed) = observe(&repository).await.unwrap();
    observed.discovery.reassert_key_objects(&cwd).await.unwrap();

    // Git 位置消失：不再有证据支持已登记的仓库布局。
    tokio::fs::remove_dir_all(repository.join(".git"))
        .await
        .unwrap();
    let error = observed
        .discovery
        .reassert_key_objects(&cwd)
        .await
        .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<WorkspaceError>(),
        Some(WorkspaceError::NeedsRelink)
    ));

    // 根目录被新的文件对象替换：路径可用不等于身份通过。
    tokio::fs::rename(&repository, directory.path().join("moved"))
        .await
        .unwrap();
    tokio::fs::create_dir(&repository).await.unwrap();
    let error = observed
        .discovery
        .reassert_key_objects(&cwd)
        .await
        .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<WorkspaceError>(),
        Some(WorkspaceError::NeedsRelink)
    ));

    // cwd 本身消失：不可用，而不是被当成布局变化。
    tokio::fs::remove_dir_all(&repository).await.unwrap();
    let error = observed
        .discovery
        .reassert_key_objects(&cwd)
        .await
        .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<WorkspaceError>(),
        Some(WorkspaceError::Unavailable)
    ));
}

#[tokio::test]
async fn test_worktree_git_missing_mid_discovery_is_not_directory_mode() {
    let directory = tempfile::tempdir().unwrap();
    let error = git_paths(
        directory.path().join("missing-git").as_os_str(),
        directory.path(),
        &["rev-parse", "--absolute-git-dir"],
        1,
    )
    .await
    .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<WorkspaceError>(),
        Some(WorkspaceError::DiscoveryError(message)) if message == "Git became unavailable during discovery"
    ));
}
