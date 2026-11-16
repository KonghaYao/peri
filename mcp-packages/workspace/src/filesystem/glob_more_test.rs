#[test]
fn test_narrow_root_blacklisted_base_falls_back() {
    // base 名黑名单保真：cwd=node_modules + `src/**/*.rs` 现状由 depth 0 黑名单
    // 检查拒绝（No files found.）；收窄必须回退，否则漂移为返回文件。
    let base = Path::new("/repo/node_modules");
    assert!(crate::filesystem::glob::narrow_root(
        base,
        &crate::filesystem::glob::plan_walk("src/**/*.rs")
    )
    .is_none());
    // 非黑名单 base 名不受影响，照常收窄
    let (root, _) = crate::filesystem::glob::narrow_root(
        Path::new("/repo/src"),
        &crate::filesystem::glob::plan_walk("deep/*.rs"),
    )
    .unwrap();
    assert_eq!(root, Path::new("/repo/src/deep"));
}

#[test]
fn test_narrow_root_blacklisted_tail_still_narrows() {
    // 末段黑名单仍收窄：收窄后 filter 的 depth 0 黑名单检查自动拒绝，
    // 行为与全遍历一致（坑 2 机制，无需回退）。
    let base = Path::new("/repo");
    let (root, consumed) = crate::filesystem::glob::narrow_root(
        base,
        &crate::filesystem::glob::plan_walk("target/**/*.rs"),
    )
    .unwrap();
    assert_eq!(root, Path::new("/repo/target"));
    assert_eq!(consumed, 1);
}

// ─── 匹配语义锁定（黑盒）────────────────────────────────────────

#[tokio::test]
async fn test_glob_star_matches_files_at_any_depth() {
    // 锁定当前 glob::Pattern::matches 语义：`*` 跨 `/`，`*.rs` 也会命中嵌套文件。
    // 若未来改为 require_literal_separator=true（非跨段），此测试需随契约同步更新。
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), "").unwrap();
    std::fs::create_dir_all(dir.path().join("sub")).unwrap();
    std::fs::write(dir.path().join("sub/b.rs"), "").unwrap();
    let tool = GlobFilesTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({"pattern": "*.rs"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(result.contains("a.rs"));
    assert!(
        result.contains("b.rs"),
        "`*` 跨 `/`，应命中 sub/b.rs: {result}"
    );
}

#[tokio::test]
async fn test_glob_literal_pattern_only_matches_exact_path() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("Cargo.toml"), "").unwrap();
    std::fs::create_dir_all(dir.path().join("sub")).unwrap();
    std::fs::write(dir.path().join("sub/Cargo.toml"), "").unwrap();
    let tool = GlobFilesTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({"pattern": "Cargo.toml"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    // Windows 绝对路径使用 \ 分隔符，统一规范化为 / 再断言
    let normalized = result.replace('\\', "/");
    assert!(
        normalized.contains("/Cargo.toml"),
        "应命中根层 Cargo.toml: {normalized}"
    );
    assert!(
        !normalized.contains("sub/Cargo.toml"),
        "字面 pattern 只应命中根层: {normalized}"
    );
}

#[tokio::test]
async fn test_glob_prefix_narrowing_finds_nested_files() {
    // 前缀剪枝（src/**/*.rs → 只下钻 src）不得改变结果集。
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src/deep")).unwrap();
    std::fs::write(dir.path().join("src/a.rs"), "").unwrap();
    std::fs::write(dir.path().join("src/deep/b.rs"), "").unwrap();
    let tool = GlobFilesTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({"pattern": "src/**/*.rs"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(result.contains("a.rs"), "应找到 src/a.rs: {result}");
    assert!(result.contains("b.rs"), "应找到 src/deep/b.rs: {result}");
}

#[tokio::test]
async fn test_glob_skips_node_modules_deep_copy() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("main.rs"), "").unwrap();
    std::fs::create_dir_all(dir.path().join("node_modules/pkg")).unwrap();
    std::fs::write(dir.path().join("node_modules/pkg/deep.rs"), "").unwrap();
    let tool = GlobFilesTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({"pattern": "**/*.rs"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(result.contains("main.rs"), "应找到 main.rs: {result}");
    assert!(
        !result.contains("deep.rs"),
        "node_modules 副本不应被扫到: {result}"
    );
}

// ─── walk root 收窄（黑盒回归）──────────────────────────────────

#[tokio::test]
async fn test_glob_narrowed_walk_skips_unrelated_dirs() {
    // 收窄后 WalkDir 从 base/src 起步：base 顶层 unrelated 目录（含大量文件）
    // 完全不进入，结果只含 src 前缀路径。
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src/deep")).unwrap();
    std::fs::write(dir.path().join("src/a.rs"), "").unwrap();
    std::fs::write(dir.path().join("src/deep/b.rs"), "").unwrap();
    // unrelated 里放大量文件 + 嵌套 src 目录，作为"被绕开"的哨兵
    std::fs::create_dir_all(dir.path().join("unrelated/src")).unwrap();
    for i in 0..200 {
        std::fs::write(dir.path().join("unrelated").join(format!("u{i}.rs")), "").unwrap();
    }
    std::fs::write(dir.path().join("unrelated/src/evil.rs"), "").unwrap();
    let tool = GlobFilesTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({"pattern": "src/**/*.rs"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    // Windows 绝对路径使用 \ 分隔符，统一规范化为 / 再断言
    let normalized = result.replace('\\', "/");
    assert!(
        normalized.contains("src/a.rs"),
        "应找到 src/a.rs: {normalized}"
    );
    assert!(
        normalized.contains("src/deep/b.rs"),
        "应找到 src/deep/b.rs: {normalized}"
    );
    assert!(
        !normalized.contains("unrelated"),
        "收窄后不得返回 unrelated 下任何路径: {normalized}"
    );
    assert!(
        !normalized.contains("evil.rs"),
        "不得返回 unrelated/src/evil.rs: {normalized}"
    );
}

#[tokio::test]
async fn test_glob_narrowed_root_missing_returns_no_files() {
    // root=base/src 不存在：walkdir yield Err(NotFound)，Err 分支跳过 → 空结果，
    // 与全遍历语义逐字节一致（"No files found." 而非报错）。
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("docs")).unwrap();
    std::fs::write(dir.path().join("docs/x.rs"), "").unwrap();
    let tool = GlobFilesTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({"pattern": "src/**/*.rs"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert_eq!(result, "No files found.");
}

#[tokio::test]
async fn test_glob_narrowed_root_blacklisted_skipped() {
    // root 末段是被跳目录：收窄后 depth 0 黑名单检查拒绝整个遍历 → No files found.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("target")).unwrap();
    std::fs::write(dir.path().join("target/x.rs"), "").unwrap();
    let tool = GlobFilesTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({"pattern": "target/**/*.rs"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert_eq!(result, "No files found.");
}

#[tokio::test]
async fn test_glob_narrowed_mid_segment_blacklist_falls_back() {
    // 前缀链中段是被跳目录：回退全遍历，depth 1 黑名单检查拒绝 → No files found.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("node_modules/pkg")).unwrap();
    std::fs::write(dir.path().join("node_modules/pkg/x.rs"), "").unwrap();
    let tool = GlobFilesTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({"pattern": "node_modules/pkg/*.rs"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert_eq!(result, "No files found.");
}

#[tokio::test]
async fn test_glob_narrowed_root_is_file_returns_no_files() {
    // base/src 是文件而非目录：收窄 root 为文件，walkdir 只 yield 该条目，
    // rel="src" 不匹配 `src/**/*.rs` → No files found.（与全遍历一致）
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("src"), "").unwrap();
    let tool = GlobFilesTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({"pattern": "src/**/*.rs"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert_eq!(result, "No files found.");
}

#[tokio::test]
async fn test_glob_blacklisted_base_keeps_no_files() {
    // base 名黑名单保真：cwd=node_modules + `src/**/*.rs` 必须仍是 No files found.
    // （若收窄不补查 base 名，会漂移为返回 node_modules/src 下的文件）
    let dir = tempfile::tempdir().unwrap();
    let nm = dir.path().join("node_modules");
    std::fs::create_dir_all(nm.join("src")).unwrap();
    std::fs::write(nm.join("src/a.rs"), "").unwrap();
    let tool = GlobFilesTool::new(nm.to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({"pattern": "src/**/*.rs"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert_eq!(result, "No files found.");
}

#[tokio::test]
async fn test_glob_narrowed_literal_multisegment() {
    // 纯字面多段 pattern：收窄 root=base/a/b，max_depth 相对化（2-2=0），
    // 文件在 root 下第一层照常 yield。
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("a/b")).unwrap();
    std::fs::write(dir.path().join("a/b/c.rs"), "").unwrap();
    std::fs::write(dir.path().join("a/b/other.rs"), "").unwrap();
    let tool = GlobFilesTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({"pattern": "a/b/c.rs"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    // Windows 绝对路径使用 \ 分隔符，统一规范化为 / 再断言
    let normalized = result.replace('\\', "/");
    assert!(
        normalized.contains("a/b/c.rs"),
        "应命中 a/b/c.rs: {normalized}"
    );
    assert!(
        !normalized.contains("other.rs"),
        "字面 pattern 不应命中同层其他文件: {normalized}"
    );
}

#[tokio::test]
async fn test_glob_narrowed_multisegment_prefix_matches() {
    // 多段前缀收窄（root=base/a/b）后匹配语义不变：`*` 仍跨 `/` 命中深层文件。
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("a/b/deep")).unwrap();
    std::fs::write(dir.path().join("a/b/x.rs"), "").unwrap();
    std::fs::write(dir.path().join("a/b/deep/y.rs"), "").unwrap();
    std::fs::write(dir.path().join("a/other.rs"), "").unwrap();
    let tool = GlobFilesTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({"pattern": "a/b/*.rs"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    // Windows 绝对路径使用 \ 分隔符，统一规范化为 / 再断言
    let normalized = result.replace('\\', "/");
    assert!(
        normalized.contains("a/b/x.rs"),
        "应命中 a/b/x.rs: {normalized}"
    );
    assert!(
        normalized.contains("a/b/deep/y.rs"),
        "`*` 跨段应命中 deep/y.rs: {normalized}"
    );
    assert!(
        !normalized.contains("a/other.rs"),
        "前缀链之外的路径不应返回: {normalized}"
    );
}

// ─── symlink 边界（unix）────────────────────────────────────────

#[cfg(unix)]
#[tokio::test]
async fn test_glob_does_not_follow_symlinked_dirs() {
    use std::os::unix::fs;
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("real")).unwrap();
    std::fs::write(dir.path().join("real/inner.rs"), "").unwrap();
    // 指向树内目录的 symlink
    fs::symlink(dir.path().join("real"), dir.path().join("link_internal")).unwrap();
    // 指向树外目录的 symlink（哨兵文件在 root 之外）
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("outside.rs"), "").unwrap();
    fs::symlink(outside.path(), dir.path().join("link_outside")).unwrap();
    let tool = GlobFilesTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({"pattern": "**/*.rs"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(
        result.contains("real/inner.rs"),
        "应找到 real/inner.rs: {result}"
    );
    assert!(
        !result.contains("link_internal"),
        "不应跟随目录 symlink: {result}"
    );
    assert!(
        !result.contains("outside.rs"),
        "不应跟随树外 symlink: {result}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn test_glob_does_not_match_symlinked_files() {
    use std::os::unix::fs;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("real.rs"), "").unwrap();
    fs::symlink(dir.path().join("real.rs"), dir.path().join("alias.rs")).unwrap();
    let tool = GlobFilesTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({"pattern": "*.rs"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(result.contains("real.rs"), "应找到 real.rs: {result}");
    assert!(
        !result.contains("alias.rs"),
        "symlink 文件不应被匹配: {result}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn test_glob_symlink_loop_terminates() {
    use std::os::unix::fs;
    let dir = tempfile::tempdir().unwrap();
    // 自指 symlink 环：walk 必须正常结束，不得挂死（防回归）。
    fs::symlink(dir.path(), dir.path().join("loop")).unwrap();
    std::fs::write(dir.path().join("a.rs"), "").unwrap();
    let tool = GlobFilesTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({"pattern": "**/*.rs"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(result.contains("a.rs"), "应找到 a.rs: {result}");
}

// ─── 排序与截断（确定性，显式 mtime，不依赖墙钟）────────────────

#[tokio::test]
async fn test_glob_results_sorted_by_mtime_descending() {
    let dir = tempfile::tempdir().unwrap();
    for (name, secs) in [
        ("old.rs", 1_600_000_000i64),
        ("mid.rs", 1_600_000_100i64),
        ("new.rs", 1_600_000_200i64),
    ] {
        let p = dir.path().join(name);
        std::fs::write(&p, "").unwrap();
        filetime::set_file_mtime(&p, filetime::FileTime::from_unix_time(secs, 0)).unwrap();
    }
    let tool = GlobFilesTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({"pattern": "*.rs"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    let lines: Vec<&str> = result.lines().collect();
    assert_eq!(lines.len(), 3, "应返回 3 个文件: {result}");
    assert!(lines[0].ends_with("new.rs"), "最新应排第一: {result}");
    assert!(lines[1].ends_with("mid.rs"), "中间应排第二: {result}");
    assert!(lines[2].ends_with("old.rs"), "最旧应排最后: {result}");
}

#[tokio::test]
async fn test_glob_truncation_sorted_and_stops_at_limit() {
    let dir = tempfile::tempdir().unwrap();
    // 2001 个文件，mtime 与序号单调递增（显式设置，不依赖墙钟）。
    for i in 0..2001i64 {
        let p = dir.path().join(format!("file_{i:04}.rs"));
        std::fs::write(&p, "").unwrap();
        filetime::set_file_mtime(&p, filetime::FileTime::from_unix_time(1_600_000_000 + i, 0))
            .unwrap();
    }
    let tool = GlobFilesTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({"pattern": "*.rs"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(
        result.contains("Output truncated"),
        "应显示截断信息: {result}"
    );
    assert!(
        result.contains("collection stopped at the result limit"),
        "应说明提前停止: {result}"
    );
    // 截断段之前恰好 1000 行，且按 mtime 降序（序号大 = mtime 新）。
    // 注意 lines() 会把 notice 前的空行也吐出来，需过滤。
    let body: Vec<&str> = result
        .lines()
        .take_while(|l| !l.contains("Output truncated"))
        .filter(|l| !l.is_empty())
        .collect();
    assert_eq!(body.len(), 1000, "应恰好 1000 行: {result}");
    let seqs: Vec<u64> = body
        .iter()
        .map(|l| {
            // 输出为平台绝对路径（Windows 用 \ 分隔），统一经 file_name 取文件名
            Path::new(l)
                .file_name()
                .unwrap()
                .to_string_lossy()
                .trim_start_matches("file_")
                .trim_end_matches(".rs")
                .parse::<u64>()
                .unwrap()
        })
        .collect();
    for w in seqs.windows(2) {
        assert!(w[0] > w[1], "应按 mtime 降序，实际 {} 在 {} 前", w[0], w[1]);
    }
}

// ─── async 不阻塞（白盒 poll，零 wall-clock）────────────────────

#[tokio::test]
async fn test_glob_invoke_yields_before_scanning() {
    let dir = tempfile::tempdir().unwrap();
    // 扫描量需足够大（ms 级）：首次 poll 断言 Pending 存在理论竞态——若主线程在
    // spawn_blocking 调度后被 OS 抢占、blocking 线程恰好先完成全部扫描，poll 会返回
    // Ready。扫描量越大，主线程首次 poll 完成前被抢占超过扫描耗时的概率越可忽略。
    for i in 0..3000 {
        std::fs::write(dir.path().join(format!("f{i}.rs")), "").unwrap();
    }
    let tool = GlobFilesTool::new(dir.path().to_str().unwrap());
    let fut = tool.invoke(
        serde_json::json!({"pattern": "**/*.rs"}),
        peri_agent::tools::ToolContext::new(&[], "."),
    );
    let mut fut = std::pin::pin!(fut);
    let waker = futures::task::noop_waker();
    let mut cx = std::task::Context::from_waker(&waker);
    // invoke 是 async_trait 方法，返回 boxed future；poll 方法来自 Future trait。
    use futures::Future;
    assert!(
        matches!(fut.as_mut().poll(&mut cx), std::task::Poll::Pending),
        "首次 poll 必须 Pending：扫描不得直接占用 async task（应移入 spawn_blocking）"
    );
    let result = fut.await;
    assert!(result.is_ok(), "await 后应正常返回: {result:?}");
}
