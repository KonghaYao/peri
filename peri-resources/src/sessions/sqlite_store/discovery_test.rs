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
        .revalidate_with_git(&cwd, missing.as_os_str())
        .await
        .unwrap();
    // 安装 Git 后，仍是普通目录时身份不变。
    discovered.revalidate(&cwd).await.unwrap();
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

/// [回归测试] Git 安装状态与布局变化不再阻断已有绑定；准入只看 cwd 是否可用。
#[tokio::test]
async fn test_worktree_git_availability_change_does_not_block_binding() {
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
    let (cwd, repository) = observe(directory.path()).await.unwrap();
    let missing = directory.path().join("missing-git");
    // Git 不可用、布局未刷新：cwd 仍在，恢复准入继续。
    repository
        .discovery
        .revalidate_with_git(&cwd, missing.as_os_str())
        .await
        .unwrap();
    let (_, plain) = observe_with_git(directory.path(), missing.as_os_str())
        .await
        .unwrap();
    assert!(!plain.git_answered);
    // 目录模式观测与仓库快照布局不同，也不影响已有绑定。
    plain.discovery.revalidate(&cwd).await.unwrap();
    // Git 恢复可用后同样只按 cwd 可用性复核。
    repository.discovery.revalidate(&cwd).await.unwrap();
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

/// [回归测试] 启动失败按有限次数退让重试：一次瞬时失败不能直接变成一次发现失败。
///
/// 首次尝试时程序还不可执行（EACCES，与权限位尚未生效同类），退让期间权限放开——
/// 这次发现必须自行恢复拿到 Git 的回答，而不是以「Git 不可执行」失败。断言只看
/// 结果，不依赖具体退让时长：权限在最后一次尝试前生效即可恢复。
#[cfg(unix)]
#[tokio::test]
async fn test_worktree_git_spawn_failure_recovers_within_retries() {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().unwrap();
    let program = directory.path().join("late-git");
    std::fs::write(
        &program,
        "#!/bin/sh\nprintf 'fatal: not a git repository' >&2\nexit 128\n",
    )
    .unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o600)).unwrap();
    let late = program.clone();
    let opener = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        std::fs::set_permissions(&late, std::fs::Permissions::from_mode(0o755)).unwrap();
    });
    let (cwd, observed) = observe_with_git(directory.path(), program.as_os_str())
        .await
        .expect("启动失败必须在同一次发现里退让重试");
    opener.await.unwrap();
    // 重试后拿到的是 Git 的回答（不是仓库），不是「Git 不可执行」。
    assert!(observed.git_answered);
    assert_eq!(observed.discovery.root, cwd);
    assert_eq!(observed.discovery.common_dir, None);
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

/// [回归测试] 旧版 Git 的「不是仓库」回答必须与新版一样被识别为目录模式。
///
/// 文案随版本变化：本机真实 Git 2.4.12 在非仓库目录回答
/// `fatal: Not a git repository (or any of the parent directories): .git`（大写 N），
/// Git 2.39 回答小写 `not`；旧版在 linked worktree 里还多一种
/// `fatal: Not a git repository: <path>`。按大小写敏感的前缀匹配会让**普通目录**在
/// 旧版 Git 下被判成类型化发现错误——用户在那个版本上根本建不了会话，而 Git 已明确
/// 回答「不是仓库」，本应得到目录项目（设计 §3.3）。
#[cfg(unix)]
#[tokio::test]
async fn test_worktree_legacy_not_a_repository_wording_is_directory_mode() {
    use std::os::unix::fs::PermissionsExt;
    for message in [
        "fatal: Not a git repository (or any of the parent directories): .git",
        "fatal: Not a git repository: /tmp/repository/.git/worktrees/linked",
    ] {
        let directory = tempfile::tempdir().unwrap();
        let program = directory.path().join("git");
        std::fs::write(
            &program,
            format!("#!/bin/sh\nprintf '{message}' >&2\nexit 128\n"),
        )
        .unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let (cwd, observed) = observe_with_git(directory.path(), program.as_os_str())
            .await
            .unwrap_or_else(|error| panic!("旧版文案 {message:?} 不应报错：{error:?}"));
        assert!(observed.git_answered, "Git 给了明确回答：{message:?}");
        assert_eq!(observed.discovery.root, cwd, "{message:?}");
        assert_eq!(observed.discovery.common_dir, None, "{message:?}");
        assert_eq!(observed.discovery.private_dir, None, "{message:?}");
    }
}

/// [回归测试] 挂起的 Git 在单次调用预算内以类型化错误结束，不无限等待。
///
/// 静态上限是「单次 5s × 每轮调用数」（仓库模式两轮共 6 次 ⇒ 30s）。这里验证的是没有
/// 走到那个上限：第一次调用不返回就以类型化错误结束。断言给的是实测耗时，静态上限只用
/// 来界定实测值是否越界，不作为耗时证据。
#[cfg(unix)]
#[tokio::test]
async fn test_worktree_hanging_git_ends_within_the_call_budget() {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().unwrap();
    let program = directory.path().join("git");
    // `exec` 让脚本自身变成挂起的进程，超时终止时不会留下后代进程。
    std::fs::write(&program, "#!/bin/sh\nexec sleep 600\n").unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    let started = std::time::Instant::now();
    let error = observe_with_git(directory.path(), program.as_os_str())
        .await
        .unwrap_err();
    let measured = started.elapsed();
    println!("挂起 Git 实测：{measured:?} 后以类型化错误结束（单次预算 5s）");
    assert!(matches!(
        error.downcast_ref::<WorkspaceError>(),
        Some(WorkspaceError::DiscoveryError(message)) if message == "Git discovery timed out"
    ));
    assert!(
        measured >= std::time::Duration::from_secs(5),
        "实测 {measured:?} 短于单次预算：没有真正触发超时路径",
    );
    assert!(
        measured < std::time::Duration::from_secs(30),
        "实测 {measured:?} 达到仓库模式的静态上限（6 次调用 × 5s）：超时没有在首次挂起时结束",
    );
}

/// [回归测试] 提交前的轻量复核只要求执行目录仍可用。
///
/// 文件对象身份与 Git 布局是诊断快照，不再参与准入：`.git` 消失、同路径换成新对象
/// 都放行。cwd 不存在或不是目录 → `Unavailable`；cwd 被 symlink/canonical 换位
/// （canonical 文本与保存路径不一致）→ `NeedsRelink`。
#[tokio::test]
async fn test_worktree_reassertion_requires_existing_cwd() {
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

    // Git 位置消失：cwd 仍在，准入通过（快照只作诊断）。
    tokio::fs::remove_dir_all(repository.join(".git"))
        .await
        .unwrap();
    observed.discovery.reassert_key_objects(&cwd).await.unwrap();

    // 根目录被新的文件对象替换：路径可用即可。
    tokio::fs::rename(&repository, directory.path().join("moved"))
        .await
        .unwrap();
    tokio::fs::create_dir(&repository).await.unwrap();
    observed.discovery.reassert_key_objects(&cwd).await.unwrap();

    // cwd 本身消失：不可用。
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
        &["rev-parse", "--show-toplevel"],
        1,
    )
    .await
    .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<WorkspaceError>(),
        Some(WorkspaceError::DiscoveryError(message)) if message == "Git became unavailable during discovery"
    ));
}

/// 用真实 Git 准备 fixture，环境隔离与其它发现用例一致。
async fn git_fixture(cwd: &Path, home: &Path, args: &[&str]) {
    git_stdout(cwd, home, args).await;
}

/// 真实 Git 的 stdout：fixture 与 oracle 共用同一套环境隔离。
async fn git_stdout(cwd: &Path, home: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .await
        .unwrap();
    assert!(output.status.success(), "Git fixture 失败: {output:?}");
    String::from_utf8(output.stdout).unwrap()
}

/// 真实 Git 报告的仓库位置：`--show-toplevel` / `--git-common-dir` / `--git-dir`。
///
/// 相对输出按 cwd 还原（与发现实现同一规则），作为发现结果的独立 oracle。
async fn git_reported_locations(cwd: &Path, home: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let reported = git_stdout(
        cwd,
        home,
        &[
            "rev-parse",
            "--show-toplevel",
            "--git-common-dir",
            "--git-dir",
        ],
    )
    .await;
    let mut lines = reported.lines();
    let root = resolve_reported(cwd, lines.next().unwrap()).await;
    let common = resolve_reported(cwd, lines.next().unwrap()).await;
    let private = resolve_reported(cwd, lines.next().unwrap()).await;
    (root, common, private)
}

async fn resolve_reported(cwd: &Path, line: &str) -> PathBuf {
    let path = Path::new(line);
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    tokio::fs::canonicalize(path).await.unwrap()
}

/// 建一个带基础提交的仓库：`git worktree add` 需要有效的 HEAD。
async fn committed_repository(home: &Path) -> PathBuf {
    let repository = home.join("repository");
    tokio::fs::create_dir(&repository).await.unwrap();
    git_fixture(&repository, home, &["init", "-q"]).await;
    git_fixture(
        &repository,
        home,
        &[
            "-c",
            "user.name=fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-qm",
            "base",
        ],
    )
    .await;
    repository
}

/// [回归测试] common directory 必须等于真实 Git 报告的位置。
///
/// 发现不再请求 `--git-common-dir`（Git 2.5 起才有），改为解析 Git 自己写入的
/// `commondir` 文件；这个文件正是 `$GIT_COMMON_DIR` 的来源。用例以真实 Git 的输出
/// 为 oracle，覆盖主仓库、子目录与 linked worktree——linked worktree 的两个位置
/// 不同，是这个改动的关键分支。
#[tokio::test]
async fn test_worktree_common_dir_matches_git_reported_locations() {
    let home = tempfile::tempdir().unwrap();
    let repository = committed_repository(home.path()).await;
    let nested = repository.join("nested space");
    tokio::fs::create_dir(&nested).await.unwrap();
    let linked = home.path().join("linked tree");
    git_fixture(
        &repository,
        home.path(),
        &["worktree", "add", "-qb", "linked", linked.to_str().unwrap()],
    )
    .await;

    for cwd in [&repository, &nested, &linked] {
        let (_, observed) = observe(cwd).await.unwrap();
        let (root, common, private) = git_reported_locations(cwd, home.path()).await;
        assert_eq!(
            observed.discovery.root, root,
            "根目录必须与 Git 报告一致：{cwd:?}"
        );
        assert_eq!(
            observed.discovery.common_dir.as_deref(),
            Some(common.as_path()),
            "common directory 必须与 Git 报告一致：{cwd:?}"
        );
        assert_eq!(
            observed.discovery.private_dir.as_deref(),
            Some(private.as_path()),
            "private directory 必须与 Git 报告一致：{cwd:?}"
        );
    }

    // linked worktree 的两个位置确实不同，否则上面的比较没有覆盖到关键分支。
    let (_, linked_observed) = observe(&linked).await.unwrap();
    assert_ne!(
        linked_observed.discovery.common_dir, linked_observed.discovery.private_dir,
        "linked worktree 的 common / private 位置必须分开解析"
    );
}

/// [回归测试] absorbed submodule 的检出必须可被发现。
///
/// submodule 的 Git 目录位于 superproject 的 `.git/modules/...` 下，`worktree list` 把
/// 主工作树报告为 Git 目录本身，检出路径永不出现在列表里（Git 2.39 实测）。成员核对若
/// 只接受检出路径，这类目录就完全建不了会话，与设计 §3.3「嵌套仓库、submodule 使用
/// cwd 所属的最近 Git 仓库」冲突。
#[tokio::test]
async fn test_worktree_submodule_checkout_is_discoverable() {
    let home = tempfile::tempdir().unwrap();
    let source = committed_repository(home.path()).await;
    let superproject = home.path().join("superproject");
    tokio::fs::create_dir(&superproject).await.unwrap();
    git_fixture(&superproject, home.path(), &["init", "-q"]).await;
    git_fixture(
        &superproject,
        home.path(),
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            source.to_str().unwrap(),
            "prototypes/p0/turso",
        ],
    )
    .await;

    let checkout = tokio::fs::canonicalize(superproject.join("prototypes/p0/turso"))
        .await
        .unwrap();
    let (cwd, observed) = observe(&checkout).await.unwrap();
    assert_eq!(cwd, checkout);
    assert_eq!(observed.discovery.root, checkout);
    let (root, common, private) = git_reported_locations(&checkout, home.path()).await;
    assert_eq!(observed.discovery.root, root);
    assert_eq!(
        observed.discovery.common_dir.as_deref(),
        Some(common.as_path())
    );
    assert_eq!(
        observed.discovery.private_dir.as_deref(),
        Some(private.as_path())
    );
    // 用例必须真的落在「Git 目录位于工作树之外」的布局上，否则没有覆盖到这个分支。
    assert!(
        !private.starts_with(&checkout),
        "submodule 的 Git 目录必须在检出之外：{private:?}"
    );
    assert!(
        !common.starts_with(&checkout),
        "submodule 的 common directory 必须在检出之外：{common:?}"
    );
    observed.discovery.revalidate(&checkout).await.unwrap();
}

/// [回归测试] 接受 submodule 布局不得放过换位后元数据陈旧的 linked worktree。
///
/// 直接改名（不经 `git worktree move`）只移动了目录，登记位置仍是旧路径：`worktree list`
/// 报告旧路径，成员核对必须继续判为不一致。
#[tokio::test]
async fn test_worktree_relocated_linked_worktree_stays_inconsistent() {
    let home = tempfile::tempdir().unwrap();
    let repository = committed_repository(home.path()).await;
    let linked = home.path().join("linked tree");
    git_fixture(
        &repository,
        home.path(),
        &["worktree", "add", "-qb", "linked", linked.to_str().unwrap()],
    )
    .await;

    let moved = home.path().join("moved tree");
    std::fs::rename(&linked, &moved).unwrap();

    let error = observe(&moved).await.unwrap_err();
    assert!(matches!(
        error.downcast_ref::<WorkspaceError>(),
        Some(WorkspaceError::DiscoveryError(message)) if message == "Git worktree membership is inconsistent"
    ));
}

/// [回归测试] 裸仓库的 linked worktree：未换位可发现，换位后必须判为不一致。
///
/// 裸仓库把主工作树列成 Git 目录自己并标记 `bare`，那个路径恰好等于 common directory：
/// 成员项若不加区分地接受 Git 目录，换位后的登记位置就会被误判为通过。
#[tokio::test]
async fn test_worktree_relocated_linked_worktree_of_bare_repository_stays_inconsistent() {
    let home = tempfile::tempdir().unwrap();
    let source = committed_repository(home.path()).await;
    let bare = home.path().join("bare.git");
    git_fixture(
        home.path(),
        home.path(),
        &["init", "--bare", "-q", bare.to_str().unwrap()],
    )
    .await;
    git_fixture(
        &source,
        home.path(),
        &["push", "-q", bare.to_str().unwrap(), "HEAD:refs/heads/main"],
    )
    .await;
    let linked = home.path().join("linked tree");
    git_fixture(
        &bare,
        home.path(),
        &["worktree", "add", "-q", linked.to_str().unwrap(), "main"],
    )
    .await;
    // 未换位时本来就该可发现：排除 `bare` 项不得把这条路一起收紧。
    let linked_canonical = tokio::fs::canonicalize(&linked).await.unwrap();
    let (cwd, observed) = observe(&linked).await.unwrap();
    assert_eq!(cwd, linked_canonical);
    assert_eq!(observed.discovery.root, linked_canonical);
    // 用例必须真的落在「列表里有一条路径等于 common directory 的 bare 项」的布局上，
    // 否则没有覆盖到「Git 目录命中」这一分支。
    let (_, common, _) = git_reported_locations(&linked, home.path()).await;
    let listing = git_stdout(&linked, home.path(), &["worktree", "list", "--porcelain"]).await;
    let bare_entry = listing
        .lines()
        .zip(listing.lines().skip(1))
        .find(|(_, attribute)| *attribute == "bare")
        .map(|(line, _)| line.trim_start_matches("worktree ").to_string())
        .expect("裸仓库的列表必须含 bare 项");
    assert_eq!(
        tokio::fs::canonicalize(&bare_entry).await.unwrap(),
        common,
        "bare 项路径必须等于 common directory"
    );

    let moved = home.path().join("moved tree");
    std::fs::rename(&linked, &moved).unwrap();

    let error = observe(&moved).await.unwrap_err();
    assert!(matches!(
        error.downcast_ref::<WorkspaceError>(),
        Some(WorkspaceError::DiscoveryError(message)) if message == "Git worktree membership is inconsistent"
    ));
}

/// [回归测试] 旧版 Git 回显未知选项时不得把选项原文当成路径。
///
/// `rev-parse` 对未知的长选项不报错，而是把它当普通参数回显到 stdout（本机 Git
/// 如此，旧版同理）。位置解析若直接拿这行当路径，就会拼出一个不存在的路径，最终
/// 以「目录不可用」掩盖版本问题。位置输出必须按类型拒绝这种回显。
#[cfg(unix)]
#[tokio::test]
async fn test_worktree_path_discovery_rejects_unknown_option_echoes() {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().unwrap();
    let program = directory.path().join("echoing-git");
    std::fs::write(&program, "#!/bin/sh\nprintf '/tmp\\n--git-dir\\n'\n").unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
    let error = git_paths(
        program.as_os_str(),
        directory.path(),
        &["rev-parse", "--show-toplevel", "--git-dir"],
        2,
    )
    .await
    .unwrap_err();
    assert!(
        matches!(
            error.downcast_ref::<WorkspaceError>(),
            Some(WorkspaceError::DiscoveryError(message))
                if message == "Git location discovery returned an unknown option"
        ),
        "未知选项回显必须按类型拒绝，而不是被当成不可用的路径：{error:?}"
    );
}

/// [回归测试] 旧版 Git 也能得到完整观测：不依赖 `--git-common-dir`，也不依赖 `worktree`。
///
/// `rev-parse --git-common-dir` 与 `git worktree` 都是 Git 2.5 引入的（上游 2.5 发布
/// 说明），此前的 Git 会把未知选项当普通参数回显，并把 `worktree` 当作不存在的命令。
/// 假 Git 只放行最老的 rev-parse 选项，其余按用法错误或「不是 git 命令」回答；发现若
/// 仍依赖这些入口就拿不到观测，旧版 Git 的仓库会话会被永久阻断。本机没有真实旧版
/// 二进制，这两种回答按 Git 对未知选项与未知命令的实际输出模拟。
#[cfg(unix)]
#[tokio::test]
async fn test_worktree_ancient_git_needs_no_common_dir_option_or_worktree_command() {
    let home = tempfile::tempdir().unwrap();
    let repository = committed_repository(home.path()).await;
    let nested = repository.join("nested space");
    tokio::fs::create_dir(&nested).await.unwrap();
    let linked = home.path().join("linked tree");
    git_fixture(
        &repository,
        home.path(),
        &["worktree", "add", "-qb", "linked", linked.to_str().unwrap()],
    )
    .await;

    let log = home.path().join("ancient-git.log");
    let preamble = r#"case "$3" in
  rev-parse)
    case "$*" in
      *--git-common-dir*|*--path-format*|*--absolute-git-dir*)
        echo "error: unknown option" >&2
        echo "usage: git rev-parse" >&2
        exit 129;;
    esac;;
  worktree)
    echo "git: 'worktree' is not a git command. See 'git --help'." >&2
    exit 1;;
esac"#;
    let program = git_shim(home.path(), "ancient-git", preamble, &log);

    for cwd in [&repository, &nested, &linked] {
        let (_, ancient) = observe_with_git(cwd, program.as_os_str()).await.unwrap();
        let (_, real) = observe(cwd).await.unwrap();
        assert!(
            ancient.git_answered,
            "旧版 Git 回答了发现，必须按仓库模式观测：{cwd:?}"
        );
        assert_eq!(
            ancient.discovery, real.discovery,
            "旧版 Git 的观测必须与真实 Git 完全一致：{cwd:?}"
        );
    }

    let invoked = std::fs::read_to_string(&log).unwrap();
    assert!(
        !invoked.contains("--git-common-dir"),
        "发现不得依赖 Git 2.5 才有的 --git-common-dir：{invoked}"
    );
    assert_eq!(
        invoked.lines().count(),
        9,
        "三次观测各有三条调用（两条 rev-parse 与一次成员探测）：{invoked}"
    );
}

#[cfg(unix)]
fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}

/// 真实 Git 的绝对路径：假 Git 把与版本无关的调用转交给它。
#[cfg(unix)]
fn real_git() -> std::path::PathBuf {
    let output = std::process::Command::new("sh")
        .args(["-c", "command -v git"])
        .output()
        .unwrap();
    assert!(output.status.success(), "测试环境需要真实 Git");
    std::path::PathBuf::from(String::from_utf8(output.stdout).unwrap().trim())
}

/// 在受控环境里写一个假 Git 脚本，记录调用后转交真实 Git。
#[cfg(unix)]
fn git_shim(directory: &Path, name: &str, preamble: &str, log: &Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let program = directory.join(name);
    std::fs::write(
        &program,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {log}\n{preamble}\nexec {real} \"$@\"\n",
            log = shell_quote(log),
            real = shell_quote(&real_git()),
        ),
    )
    .unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
    program
}

/// [回归测试] 旧版 Git 仍能完成仓库发现：只用 Git 2.7 起就有的选项。
///
/// 上一版发现依赖 `rev-parse --path-format=absolute`（Git 2.31）与
/// `worktree list --porcelain -z`（更晚才出现）。旧版 Git 把它们当未知选项并按用法
/// 错误退出，于是带 `.git` 的目录被判成无法发现——用户在任何仓库里都建不了会话。
/// 假 Git 拒绝这些选项、其余转交真实 Git，用来证明发现不依赖版本相关选项，且退回
/// 之后得到与真实 Git 完全相同的路径与身份。
#[cfg(unix)]
#[tokio::test]
async fn test_worktree_legacy_git_discovers_repository_without_version_options() {
    let directory = tempfile::tempdir().unwrap();
    let repository = directory.path().join("repository");
    tokio::fs::create_dir(&repository).await.unwrap();
    git_fixture(&repository, directory.path(), &["init", "-q"]).await;
    git_fixture(
        &repository,
        directory.path(),
        &[
            "-c",
            "user.name=fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-qm",
            "base",
        ],
    )
    .await;
    let linked = directory.path().join("linked tree");
    git_fixture(
        &repository,
        directory.path(),
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "linked",
            linked.to_str().unwrap(),
        ],
    )
    .await;
    let nested = linked.join("nested space");
    tokio::fs::create_dir(&nested).await.unwrap();

    // 独立 oracle：同一批目录由真实 Git 得到的观测。
    let (_, main) = observe(&repository).await.unwrap();
    let (_, linked_real) = observe(&linked).await.unwrap();
    let (_, nested_real) = observe(&nested).await.unwrap();

    let log = directory.path().join("legacy-git.log");
    let preamble = "for arg in \"$@\"; do\n  case \"$arg\" in\n    --path-format=*|-z)\n      echo \"error: unknown option $arg\" >&2\n      echo usage: git >&2\n      exit 129;;\n  esac\ndone";
    let program = git_shim(directory.path(), "legacy-git", preamble, &log);
    let (_, main_legacy) = observe_with_git(&repository, program.as_os_str())
        .await
        .unwrap();
    let (_, linked_legacy) = observe_with_git(&linked, program.as_os_str())
        .await
        .unwrap();
    let (_, nested_legacy) = observe_with_git(&nested, program.as_os_str())
        .await
        .unwrap();

    assert!(
        main_legacy.git_answered && linked_legacy.git_answered && nested_legacy.git_answered,
        "旧版 Git 回答了发现，必须按仓库模式观测"
    );
    assert_eq!(
        main_legacy.discovery, main.discovery,
        "主仓库的路径与身份不得因旧版 Git 变化"
    );
    assert_eq!(
        linked_legacy.discovery, linked_real.discovery,
        "linked worktree 的位置不得退化"
    );
    assert_eq!(
        nested_legacy.discovery, nested_real.discovery,
        "子目录工作区不得退化"
    );
    assert_ne!(
        linked_legacy.discovery.common_dir, linked_legacy.discovery.private_dir,
        "common directory 与私有目录仍须分开解析"
    );

    let invoked = std::fs::read_to_string(&log).unwrap();
    assert!(
        !invoked.contains("--path-format"),
        "发现不得依赖 Git 2.31 才有的 --path-format：{invoked}"
    );
    assert!(
        invoked.contains("--porcelain -z"),
        "支持 NUL 分隔的 Git 仍优先用 -z：{invoked}"
    );
    assert!(
        invoked.lines().any(|line| line.ends_with("--porcelain")),
        "旧版 Git 退回换行分隔：{invoked}"
    );
}

/// [回归测试] `rev-parse` 的默认输出相对 cwd：必须还原成仓库的绝对位置。
///
/// 兼容调用取代 `--path-format=absolute`（Git 2.31）之后，`--git-common-dir` 会按
/// cwd 输出相对路径（主仓库子目录里是 `../../.git`），而同一位置的 `--git-dir`
/// 又可能是绝对路径——两类输出混排，只有逐个与 cwd 组合才能得到同一个仓库位置。
/// 直接断言具体路径，不拿同一实现的结果互为 oracle。
#[tokio::test]
async fn test_worktree_git_paths_resolve_relative_output_against_cwd() {
    let directory = tempfile::tempdir().unwrap();
    let repository = directory.path().join("repository");
    tokio::fs::create_dir(&repository).await.unwrap();
    git_fixture(&repository, directory.path(), &["init", "-q"]).await;
    let nested = repository.join("nested space");
    tokio::fs::create_dir(&nested).await.unwrap();
    let root = tokio::fs::canonicalize(&repository).await.unwrap();
    let git_dir = root.join(".git");

    let (_, from_root) = observe(&repository).await.unwrap();
    let (_, from_nested) = observe(&nested).await.unwrap();
    assert_eq!(
        from_nested.discovery, from_root.discovery,
        "子目录与仓库根必须观测到同一份布局"
    );
    assert_eq!(from_nested.discovery.root, root);
    assert_eq!(
        from_nested.discovery.common_dir.as_deref(),
        Some(git_dir.as_path()),
        "相对 cwd 的 --git-common-dir 输出必须按 cwd 还原"
    );
    assert_eq!(
        from_nested.discovery.private_dir.as_deref(),
        Some(git_dir.as_path()),
        "绝对输出的 --git-dir 不得再被按 cwd 拼接"
    );
}

/// [回归测试] 退回只针对用法错误：真实的 Git 失败不得被当成旧版 Git 重试。
#[cfg(unix)]
#[tokio::test]
async fn test_worktree_listing_failure_is_not_retried_as_legacy_git() {
    let directory = tempfile::tempdir().unwrap();
    let repository = directory.path().join("repository");
    tokio::fs::create_dir(&repository).await.unwrap();
    git_fixture(&repository, directory.path(), &["init", "-q"]).await;
    let log = directory.path().join("failing-git.log");
    let preamble = "case \"$*\" in\n  *\"worktree list\"*) echo \"fatal: could not read worktrees\" >&2; exit 128;;\nesac";
    let program = git_shim(directory.path(), "failing-git", preamble, &log);
    let error = observe_with_git(&repository, program.as_os_str())
        .await
        .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<WorkspaceError>(),
        Some(WorkspaceError::DiscoveryError(message)) if message == "Git worktree membership is inconsistent"
    ));
    let invoked = std::fs::read_to_string(&log).unwrap();
    assert_eq!(
        invoked
            .lines()
            .filter(|line| line.contains("worktree list"))
            .count(),
        1,
        "非用法错误不得触发兼容退回：{invoked}"
    );
}
