//! `git_watch` 服务端状态的 crate 内证据（T1–T3）。
//!
//! - T1 纯逻辑：`parse_sample_stdout` 三态；`notice_text` 仅列变化项、短 hash 7 位、
//!   首采样/无变化 → `None`；`snapshot_text` 与未采样正文。
//! - T2 状态机（`with_timing` 注入）：节流窗口内不重复采样、单飞、`NotRepository`
//!   短路后不再 spawn、采样失败不推进节流、首采样不通知。
//! - T3 真实 git：`git init -b main` + `config user.*`，锁 `process_env`（与
//!   `terminal_*_test.rs` / 旧 `git_watch/mod_test.rs` 同写法）后断言首采样基线与
//!   commit 后第二次采样的 notice 文案。

use std::{process::Command as StdCommand, time::Duration};

use tempfile::tempdir;

use super::*;

fn init_git_repo(path: &std::path::Path) {
    StdCommand::new("git")
        .args(["init", "-b", "main"])
        .current_dir(path)
        .output()
        .expect("git init");
    StdCommand::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(path)
        .output()
        .expect("git config email");
    StdCommand::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(path)
        .output()
        .expect("git config name");
    std::fs::write(path.join("README.md"), "hi").unwrap();
    StdCommand::new("git")
        .args(["add", "README.md"])
        .current_dir(path)
        .output()
        .expect("git add");
    StdCommand::new("git")
        .args(["commit", "-m", "init"])
        .current_dir(path)
        .output()
        .expect("git commit");
}

fn snapshot(branch: &str, head: &str) -> GitSnapshot {
    GitSnapshot {
        branch: branch.to_string(),
        head: head.to_string(),
    }
}

// ─── T1 纯逻辑 ────────────────────────────────────────────────────────────────

#[test]
fn parse_sample_stdout_three_states() {
    match parse_sample_stdout("true\nabc123def456789\nmain\n") {
        SampleOutcome::Repository(snapshot) => {
            assert_eq!(snapshot.branch, "main");
            assert_eq!(snapshot.head, "abc123def456789");
        }
        other => panic!("expected Repository, got {other:?}"),
    }
    assert_eq!(
        parse_sample_stdout("false\n"),
        SampleOutcome::NotRepository,
        "非仓库态必须与失败区分"
    );
    assert_eq!(parse_sample_stdout(""), SampleOutcome::Failed);
    assert_eq!(
        parse_sample_stdout("true\nabc123\n"),
        SampleOutcome::Failed,
        "缺 branch 行即失败"
    );
}

#[test]
fn notice_text_lists_only_changed_fields_with_short_hash() {
    let a = snapshot("main", &"a".repeat(40));
    assert!(notice_text(None, &a).is_none(), "首采样没有基线 ⇒ 不通知");
    assert!(notice_text(Some(&a), &a).is_none(), "无变化 ⇒ 不通知");

    let branch_changed = snapshot("dev", &a.head);
    let msg = notice_text(Some(&a), &branch_changed).expect("branch 变化必须产出 notice");
    assert!(msg.contains("- Branch: main → dev"));
    assert!(!msg.contains("HEAD:"), "未变化项不出现：{msg}");

    let head_changed = snapshot("dev", &"b".repeat(40));
    let msg = notice_text(Some(&branch_changed), &head_changed).expect("HEAD 变化必须产出 notice");
    assert!(
        msg.contains("- HEAD: aaaaaaa → bbbbbbb"),
        "短 hash 取 7 位：{msg}"
    );
    assert!(!msg.contains("Branch:"), "未变化项不出现：{msg}");
    assert!(msg.contains("[Git watch] Repository ref changed since the last sample:"));
    assert!(msg.contains("Run `git status` and `git log -1`"));
}

#[test]
fn snapshot_text_and_unsampled_body() {
    let state = GitWatchState::new();
    assert_eq!(
        state.resource_text(),
        UNSAMPLED_RESOURCE_TEXT,
        "未采样时的资源正文是固定文本"
    );

    let text = snapshot_text(&snapshot("main", &"c".repeat(40)));
    assert!(text.contains("[Git watch] Repository ref snapshot:"));
    assert!(text.contains("- Branch: main"));
    assert!(text.contains("- HEAD: ccccccc"));
    assert!(!text.contains("→"), "快照文本不含箭头：{text}");
}

// ─── T2 状态机 ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn throttle_blocks_resampling_within_window() {
    let state = GitWatchState::with_timing(Duration::from_secs(60), GIT_SAMPLE_TIMEOUT);
    assert!(state.begin_sample(), "首次触发可采样");
    assert!(
        state
            .finish_sample(SampleOutcome::Repository(snapshot("main", "head1")))
            .is_none(),
        "首采样不通知"
    );
    assert!(!state.begin_sample(), "完成时刻起 60s 窗口内不得重复采样");
}

#[tokio::test]
async fn zero_throttle_allows_resampling_and_no_change_does_not_notify() {
    let state = GitWatchState::with_timing(Duration::ZERO, GIT_SAMPLE_TIMEOUT);
    assert!(state.begin_sample());
    assert!(state
        .finish_sample(SampleOutcome::Repository(snapshot("main", "head1")))
        .is_none());
    assert!(state.begin_sample(), "零节流窗口允许再次采样");
    assert!(
        state
            .finish_sample(SampleOutcome::Repository(snapshot("main", "head1")))
            .is_none(),
        "同快照不通知"
    );
}

#[tokio::test]
async fn in_flight_blocks_concurrent_sample() {
    let state = GitWatchState::with_timing(Duration::ZERO, GIT_SAMPLE_TIMEOUT);
    assert!(state.begin_sample());
    assert!(!state.begin_sample(), "采样在途时第二次触发必须被单飞挡住");
    state.finish_sample(SampleOutcome::Repository(snapshot("main", "head1")));
    assert!(state.begin_sample(), "收口后恢复可触发");
}

#[tokio::test]
async fn not_repository_short_circuits_forever() {
    let state = GitWatchState::with_timing(Duration::ZERO, GIT_SAMPLE_TIMEOUT);
    assert!(state.begin_sample());
    assert!(state.finish_sample(SampleOutcome::NotRepository).is_none());
    assert!(
        !state.begin_sample(),
        "NotRepository 首次短路后不得再 spawn（零节流窗口下也必须为假）"
    );
}

#[tokio::test]
async fn failed_sample_does_not_advance_throttle() {
    let state = GitWatchState::with_timing(Duration::from_secs(60), GIT_SAMPLE_TIMEOUT);
    assert!(state.begin_sample());
    assert!(state.finish_sample(SampleOutcome::Failed).is_none());
    assert!(
        state.begin_sample(),
        "采样失败不推进节流 ⇒ 下一次触发立即可再试"
    );
}

#[tokio::test]
async fn change_updates_resource_body_with_notice() {
    let state = GitWatchState::with_timing(Duration::ZERO, GIT_SAMPLE_TIMEOUT);
    state.begin_sample();
    state.finish_sample(SampleOutcome::Repository(snapshot("main", &"a".repeat(40))));
    assert!(
        state.resource_text().contains("Repository ref snapshot:"),
        "无变化时资源正文是快照文本"
    );

    state.begin_sample();
    let notice = state
        .finish_sample(SampleOutcome::Repository(snapshot("dev", &"b".repeat(40))))
        .expect("变化必须返回 notice");
    assert_eq!(
        state.resource_text(),
        notice,
        "资源正文 = notice 逐字（提醒正文与资源正文同源）"
    );
}

// ─── T3 真实 git ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn real_repo_baseline_then_commit_notice() {
    let _process_env = peri_mcp_common::process_env::lock().expect("process env lock");
    let dir = tempdir().unwrap();
    init_git_repo(dir.path());
    let cwd = dir.path().to_string_lossy().into_owned();

    let state = GitWatchState::with_timing(Duration::ZERO, GIT_SAMPLE_TIMEOUT);
    assert!(state.begin_sample());
    let first = run_git_sample(&cwd).await;
    match &first {
        SampleOutcome::Repository(snapshot) => assert_eq!(snapshot.branch, "main"),
        other => panic!("首采样必须命中仓库，实际 {other:?}"),
    }
    assert!(
        state.finish_sample(first).is_none(),
        "首采样只建基线，不通知"
    );

    std::fs::write(dir.path().join("README.md"), "changed").unwrap();
    StdCommand::new("git")
        .args(["commit", "-am", "second"])
        .current_dir(dir.path())
        .output()
        .expect("git commit");

    assert!(state.begin_sample());
    let second = run_git_sample(&cwd).await;
    let notice = state
        .finish_sample(second)
        .expect("commit 后第二次采样必须产出 notice");
    assert!(notice.contains("[Git watch] Repository ref changed since the last sample:"));
    assert!(notice.contains("- HEAD:"), "{notice}");
    assert_eq!(state.resource_text(), notice);
}

#[tokio::test]
async fn real_non_repo_sample_is_failed() {
    let _process_env = peri_mcp_common::process_env::lock().expect("process env lock");
    let dir = tempdir().unwrap();
    let cwd = dir.path().to_string_lossy().into_owned();
    // 普通目录下 `git rev-parse` 以非零退出（fatal: not a git repository），旧实现的
    // 映射是 `Failed`（不是 `NotRepository`）——本用例锁这条既有事实。
    // `NotRepository`（`--is-inside-work-tree` 印出 `false` 且退出码 0）在旧实现里只由
    // 解析层可达，由 T1 的 `parse_sample_stdout` 与 T2 的状态机短路锁定。
    assert_eq!(
        run_git_sample(&cwd).await,
        SampleOutcome::Failed,
        "非仓库目录必须按失败收口（不推进节流、不通知）"
    );
}
