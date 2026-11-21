use crate::journal::git::GitBaseline;
use crate::journal::git::GitObservation;
use crate::journal::AttemptTracker;
use crate::journal::RunState;
use crate::journal::WorkflowJournalStore;
use crate::protocol::JournalEntry;
use crate::protocol::{AgentRunResult, Usage};
use peri_acp_types::workflow::AcceptanceStatus;
use peri_acp_types::workflow::AttemptDisposition;
use peri_acp_types::workflow::DeliveryStatus;
use peri_acp_types::workflow::ExecutionStatus;
use peri_acp_types::workflow::PostProcessingStatus;
use peri_acp_types::workflow::WorkflowAttempt;

use tempfile::TempDir;

fn make_store() -> (TempDir, WorkflowJournalStore) {
    let tmp = TempDir::new().expect("the new fixture result is required for this assertion");
    let store = WorkflowJournalStore::new(
        tmp.path()
            .to_str()
            .expect("the fixture text must be valid UTF-8"),
    );
    (tmp, store)
}

#[test]
fn test_init_run_creates_dir_and_script() {
    let (_tmp, store) = make_store();
    store
        .init_run("run-1", "export const meta = {}")
        .expect("the init run fixture result is required for this assertion");
    let script = std::fs::read_to_string(store.run_dir("run-1").join("script.js"))
        .expect("the test fixture text file must be readable");
    assert_eq!(script, "export const meta = {}");
}

#[test]
fn test_append_and_read_all_journal() {
    let (_tmp, store) = make_store();
    store
        .init_run("run-1", "script")
        .expect("the init run fixture result is required for this assertion");
    let entry = JournalEntry {
        key: "abc123".into(),
        seq: 0,
        result: AgentRunResult::Ok {
            output: "hello".into(),
            usage: Usage { output_tokens: 10 },
            model: None,
            tool_count: None,
            token_count: None,
            phase: None,
            duration_ms: None,
        },
        attempt: None,
    };
    store
        .append("run-1", &entry)
        .expect("the append fixture result is required for this assertion");
    let entries = store
        .read_all("run-1")
        .expect("the read all fixture result is required for this assertion");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].key, "abc123");
    assert_eq!(entries[0].seq, 0);
}

#[test]
fn strict_resume_read_rejects_missing_and_malformed_journal() {
    let (_tmp, store) = make_store();
    store
        .init_run("missing", "script")
        .expect("the init run fixture result is required for this assertion");
    assert!(store.read_all_strict("missing").is_err());

    store
        .init_run("malformed", "script")
        .expect("the init run fixture result is required for this assertion");
    std::fs::write(
        store.run_dir("malformed").join("journal.jsonl"),
        "{\"key\":\"ok\",\"seq\":0,\"result\":{\"kind\":\"ok\",\"output\":\"ok\",\"usage\":{\"outputTokens\":1}}}\nnot-json\n",
    )
    .expect("the test fixture file must be writable");
    let error = store.read_all_strict("malformed").unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("line 2"));

    store
        .init_run("gap", "script")
        .expect("the init run fixture result is required for this assertion");
    std::fs::write(
        store.run_dir("gap").join("journal.jsonl"),
        "{\"key\":\"later\",\"seq\":1,\"result\":{\"kind\":\"ok\",\"output\":\"later\",\"usage\":{\"outputTokens\":1}}}\n",
    )
    .expect("the test fixture file must be writable");
    let error = store.read_all_strict("gap").unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("expected seq 0"));

    store
        .init_run("duplicate", "script")
        .expect("the init run fixture result is required for this assertion");
    std::fs::write(
        store.run_dir("duplicate").join("journal.jsonl"),
        "{\"key\":\"a\",\"seq\":0,\"result\":{\"kind\":\"ok\",\"output\":\"a\",\"usage\":{\"outputTokens\":1}}}\n{\"key\":\"b\",\"seq\":0,\"result\":{\"kind\":\"ok\",\"output\":\"b\",\"usage\":{\"outputTokens\":1}}}\n",
    )
    .expect("the test fixture file must be writable");
    let error = store.read_all_strict("duplicate").unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("expected seq 1"));
}

#[test]
fn test_truncate_clears_journal() {
    let (_tmp, store) = make_store();
    store
        .init_run("run-1", "script")
        .expect("the init run fixture result is required for this assertion");
    let entry = JournalEntry {
        key: "k".into(),
        seq: 0,
        result: AgentRunResult::Skipped,
        attempt: None,
    };
    store
        .append("run-1", &entry)
        .expect("the append fixture result is required for this assertion");
    store
        .truncate("run-1")
        .expect("the truncate fixture result is required for this assertion");
    let entries = store
        .read_all("run-1")
        .expect("the read all fixture result is required for this assertion");
    assert!(entries.is_empty());
}

#[test]
fn test_write_and_read_state() {
    let (_tmp, store) = make_store();
    store
        .init_run("run-1", "script")
        .expect("the init run fixture result is required for this assertion");
    let state = RunState {
        run_id: "run-1".into(),
        workflow_name: "test".into(),
        status: "completed".into(),
        execution_status: ExecutionStatus::Completed,
        acceptance_status: AcceptanceStatus::Unknown,
        post_processing_status: PostProcessingStatus::Blocked,
        delivery_status: DeliveryStatus::Blocked,
        delivery_reason: None,
        postcondition_observation: None,
        git_observation_scope: None,
        outcome: peri_acp_types::workflow::WorkflowOutcome::Blocked,
        write_intent: None,
        limits: crate::protocol::WorkflowLimits::default(),
        limit_breaches: Vec::new(),
        token_usage: None,
        budget_total: Some(9_007_199_254_740_991),
        args: Some(serde_json::json!({"task": "round-trip"})),
        max_concurrency: 7,
        attempts: Vec::new(),
        return_value: Some(serde_json::json!({"ok": true})),
        script: "script".into(),
        started_at: "2026-06-22T00:00:00Z".into(),
        finished_at: Some("2026-06-22T00:01:00Z".into()),
        error: None,
    };
    store
        .write_state("run-1", &state)
        .expect("the write state fixture result is required for this assertion");
    let read = store
        .read_state("run-1")
        .expect("the read state fixture result is required for this assertion");
    assert_eq!(read.run_id, "run-1");
    assert_eq!(read.status, "completed");
    assert_eq!(read.budget_total, Some(9_007_199_254_740_991));
    assert_eq!(read.args, Some(serde_json::json!({"task": "round-trip"})));
    assert_eq!(read.max_concurrency, 7);
    assert_eq!(
        read.outcome,
        peri_acp_types::workflow::WorkflowOutcome::Blocked,
        "state.json 必须保留独立的引擎级 outcome"
    );
}

#[test]
fn test_legacy_state_defaults_budget_total_to_none() {
    let state: RunState = serde_json::from_value(serde_json::json!({
        "run_id": "legacy-budget",
        "workflow_name": "legacy",
        "status": "completed",
        "script": "script",
        "started_at": "2026-06-22T00:00:00Z"
    }))
    .expect("from_value must accept this test fixture representation");

    assert_eq!(state.budget_total, None);
    assert_eq!(state.args, None);
    assert_eq!(state.max_concurrency, 3);
}

#[test]
fn test_state_error_field_round_trip() {
    // failed 的 state.json 必须能保存并读回 error 字段
    let (_tmp, store) = make_store();
    store
        .init_run("run-err", "script")
        .expect("the init run fixture result is required for this assertion");
    let state = RunState {
        run_id: "run-err".into(),
        workflow_name: "test".into(),
        status: "failed".into(),
        execution_status: ExecutionStatus::Failed,
        acceptance_status: AcceptanceStatus::Unknown,
        post_processing_status: PostProcessingStatus::Failed,
        delivery_status: DeliveryStatus::Blocked,
        delivery_reason: None,
        postcondition_observation: None,
        git_observation_scope: None,
        outcome: peri_acp_types::workflow::WorkflowOutcome::Blocked,
        write_intent: None,
        limits: crate::protocol::WorkflowLimits::default(),
        limit_breaches: Vec::new(),
        token_usage: None,
        budget_total: None,
        args: None,
        max_concurrency: 3,
        attempts: Vec::new(),
        return_value: None,
        script: "script".into(),
        started_at: "2026-06-22T00:00:00Z".into(),
        finished_at: Some("2026-06-22T00:00:01Z".into()),
        error: Some("parallel thunk #0 failed: t is not a function".into()),
    };
    store
        .write_state("run-err", &state)
        .expect("the write state fixture result is required for this assertion");
    let read = store
        .read_state("run-err")
        .expect("the read state fixture result is required for this assertion");
    assert_eq!(read.status, "failed");
    assert_eq!(
        read.error.as_deref(),
        Some("parallel thunk #0 failed: t is not a function")
    );
}

#[test]
fn test_state_error_skipped_when_none() {
    // error 为 None 时序列化应省略该字段（向后兼容旧 state.json）
    let (_tmp, store) = make_store();
    store
        .init_run("run-ok", "script")
        .expect("the init run fixture result is required for this assertion");
    let state = RunState {
        run_id: "run-ok".into(),
        workflow_name: "test".into(),
        status: "completed".into(),
        execution_status: ExecutionStatus::Completed,
        acceptance_status: AcceptanceStatus::Passed,
        post_processing_status: PostProcessingStatus::NotRequired,
        delivery_status: DeliveryStatus::Unknown,
        delivery_reason: None,
        postcondition_observation: None,
        git_observation_scope: None,
        outcome: peri_acp_types::workflow::WorkflowOutcome::Completed,
        write_intent: None,
        limits: crate::protocol::WorkflowLimits::default(),
        limit_breaches: Vec::new(),
        token_usage: None,
        budget_total: None,
        args: None,
        max_concurrency: 3,
        attempts: Vec::new(),
        return_value: None,
        script: "script".into(),
        started_at: "2026-06-22T00:00:00Z".into(),
        finished_at: None,
        error: None,
    };
    store
        .write_state("run-ok", &state)
        .expect("the write state fixture result is required for this assertion");
    let raw = std::fs::read_to_string(store.run_dir("run-ok").join("state.json"))
        .expect("the test fixture text file must be readable");
    assert!(!raw.contains("error"), "None 时不应序列化 error 字段");
}

#[test]
fn test_legacy_deliverable_state_reads_as_unknown() {
    let state: RunState = serde_json::from_value(serde_json::json!({
        "run_id": "legacy-delivery",
        "workflow_name": "legacy",
        "status": "completed",
        "delivery_status": "deliverable",
        "script": "script",
        "started_at": "2026-06-22T00:00:00Z"
    }))
    .expect("from_value must accept this test fixture representation");

    assert_eq!(
        state.delivery_status,
        DeliveryStatus::Unknown,
        "历史 state.json 的 deliverable 必须按 unknown 读入（数据兼容），不得反序列化失败"
    );
}

#[test]
fn test_legacy_completed_defaults_do_not_claim_delivery() {
    let state: RunState = serde_json::from_value(serde_json::json!({
        "run_id": "legacy-run",
        "workflow_name": "legacy",
        "status": "completed",
        "script": "script",
        "started_at": "2026-06-22T00:00:00Z",
        "future_field": {"ignored": true}
    }))
    .expect("from_value must accept this test fixture representation");

    assert_eq!(state.status, "completed");
    assert_eq!(state.execution_status, ExecutionStatus::Unknown);
    assert_eq!(state.acceptance_status, AcceptanceStatus::Unknown);
    assert_eq!(state.post_processing_status, PostProcessingStatus::Unknown);
    assert_eq!(state.delivery_status, DeliveryStatus::Unknown);
    assert_eq!(
        state.delivery_reason, None,
        "旧 state.json 缺原因字段：不得据此推断“未请求交付”或“无变化”"
    );
    assert_eq!(state.postcondition_observation, None);
}

#[test]
fn postcondition_reason_fields_round_trip_and_stay_optional() {
    let tmp = TempDir::new().expect("the temporary test directory must be available");
    let store = WorkflowJournalStore::new(
        tmp.path()
            .to_str()
            .expect("the fixture text must be valid UTF-8"),
    );
    store
        .init_run("run-reason", "script")
        .expect("the init run fixture result is required for this assertion");
    let state: RunState = serde_json::from_value(serde_json::json!({
        "run_id": "run-reason",
        "workflow_name": "reason",
        "status": "completed",
        "script": "script",
        "started_at": "2026-06-22T00:00:00Z"
    }))
    .expect("legacy payload must deserialize");
    let mut state = state;
    state.delivery_reason = Some(peri_acp_types::workflow::DeliveryReason::NotRequested);
    state.postcondition_observation =
        Some(peri_acp_types::workflow::PostconditionObservation::Complete);
    store
        .write_state("run-reason", &state)
        .expect("the write state fixture result is required for this assertion");

    let read = store
        .read_state("run-reason")
        .expect("the read state fixture result is required for this assertion");
    assert_eq!(
        read.delivery_reason,
        Some(peri_acp_types::workflow::DeliveryReason::NotRequested)
    );
    assert_eq!(
        read.postcondition_observation,
        Some(peri_acp_types::workflow::PostconditionObservation::Complete)
    );
}

#[test]
fn test_non_git_directory_reports_not_applicable_without_intent() {
    let tmp = TempDir::new().expect("the new fixture result is required for this assertion");

    assert!(matches!(
        GitBaseline::capture_for_intent(tmp.path(), None)
            .expect("sampling failure must not fail start"),
        GitObservation::NotApplicable
    ));
    assert!(matches!(
        GitBaseline::capture_for_intent(
            tmp.path(),
            Some(&peri_acp_types::workflow::WorkflowWriteIntent::ReadOnly),
        )
        .expect("read-only sampling failure must not fail start"),
        GitObservation::NotApplicable
    ));
}

#[test]
fn test_non_git_baseline_is_required_for_write_intent() {
    let tmp = TempDir::new().expect("the new fixture result is required for this assertion");
    let intent = peri_acp_types::workflow::WorkflowWriteIntent::Write {
        repo_root: tmp.path().to_string_lossy().into_owned(),
        cwd: tmp.path().to_string_lossy().into_owned(),
        path_allowlist: vec!["allowed.txt".into()],
        head_may_change: false,
        commit_required: None,
    };

    let error = GitBaseline::capture_for_intent(tmp.path(), Some(&intent)).unwrap_err();
    // 采样失败必须保留 git 自身 stderr 摘要，不能退化成统一文本。
    let detail = error
        .strip_prefix("git rev-parse --show-toplevel failed: ")
        .unwrap_or_else(|| panic!("错误必须带失败命令: {error}"));
    assert!(
        !detail.trim().is_empty() && detail != "no stderr output",
        "错误必须保留 git 诊断: {error}"
    );
    assert!(
        detail.chars().count() <= 501,
        "stderr 摘要必须有界: {error}"
    );
}

#[test]
fn test_git_baseline_preserves_dirty_staged_and_untracked_bytes() {
    let tmp = TempDir::new().expect("the new fixture result is required for this assertion");
    let run_git = |args: &[&str]| {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(tmp.path())
            .status()
            .expect("the status fixture result is required for this assertion");
        assert!(status.success());
    };
    run_git(&["init", "-q"]);
    std::fs::write(tmp.path().join("tracked.txt"), b"base\n")
        .expect("the test fixture file must be writable");
    run_git(&["add", "tracked.txt"]);
    run_git(&[
        "-c",
        "user.name=Peri Test",
        "-c",
        "user.email=peri-test@example.invalid",
        "commit",
        "-qm",
        "baseline",
    ]);

    std::fs::write(tmp.path().join("tracked.txt"), b"staged\n")
        .expect("the test fixture file must be writable");
    run_git(&["add", "tracked.txt"]);
    std::fs::write(tmp.path().join("tracked.txt"), b"dirty\n")
        .expect("the test fixture file must be writable");
    std::fs::write(tmp.path().join("untracked.bin"), [0_u8, 1, 2, 255])
        .expect("the test fixture file must be writable");
    let tracked_before = std::fs::read(tmp.path().join("tracked.txt"))
        .expect("the test fixture file must be readable");
    let untracked_before = std::fs::read(tmp.path().join("untracked.bin"))
        .expect("the test fixture file must be readable");

    let baseline = GitBaseline::capture(tmp.path())
        .expect("the capture fixture result is required for this assertion");
    baseline
        .verify_unchanged()
        .expect("the verify unchanged fixture result is required for this assertion");

    assert_eq!(
        std::fs::read(tmp.path().join("tracked.txt"))
            .expect("the test fixture file must be readable"),
        tracked_before
    );
    assert_eq!(
        std::fs::read(tmp.path().join("untracked.bin"))
            .expect("the test fixture file must be readable"),
        untracked_before
    );
}

#[test]
fn test_git_baseline_detects_unattributed_change_without_restoring_it() {
    let tmp = TempDir::new().expect("the new fixture result is required for this assertion");
    let run_git = |args: &[&str]| {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(tmp.path())
            .status()
            .expect("the status fixture result is required for this assertion");
        assert!(status.success());
    };
    run_git(&["init", "-q"]);
    std::fs::write(tmp.path().join("tracked.txt"), "base\n")
        .expect("the test fixture file must be writable");
    run_git(&["add", "tracked.txt"]);
    run_git(&[
        "-c",
        "user.name=Peri Test",
        "-c",
        "user.email=peri-test@example.invalid",
        "commit",
        "-qm",
        "baseline",
    ]);

    let baseline = GitBaseline::capture(tmp.path())
        .expect("the capture fixture result is required for this assertion");
    std::fs::write(
        tmp.path().join("tracked.txt"),
        "agent claim without ownership\n",
    )
    .expect("the test fixture file must be writable");

    let error = baseline.verify_unchanged().unwrap_err();
    assert!(
        !error.is_violation(),
        "无 write footprint 时变化只能判为未归因"
    );
    let message = error.to_string();
    assert!(message.contains("outside the run's write footprint"));
    assert!(
        message.contains("tracked.txt"),
        "报告必须带可定位路径: {message}"
    );
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("tracked.txt"))
            .expect("the test fixture text file must be readable"),
        "agent claim without ownership\n"
    );
}

/// 未合并条目（`u`）也要能被解析：始终比较 map（而非原始字节）后，
/// 冲突中的仓库不能因为解析失败被读成观测不完整。
#[test]
fn unmerged_porcelain_entries_do_not_claim_a_change() {
    let tmp = TempDir::new().expect("the new fixture result is required for this assertion");
    let run_git = |args: &[&str]| {
        assert!(std::process::Command::new("git")
            .args(args)
            .current_dir(tmp.path())
            .status()
            .expect("the status fixture result is required for this assertion")
            .success());
    };
    run_git(&[
        "-c",
        "user.name=Peri Test",
        "-c",
        "user.email=peri-test@example.invalid",
        "init",
        "-q",
    ]);
    std::fs::write(tmp.path().join("tracked.txt"), "base\n")
        .expect("the test fixture file must be writable");
    run_git(&["add", "tracked.txt"]);
    run_git(&[
        "-c",
        "user.name=Peri Test",
        "-c",
        "user.email=peri-test@example.invalid",
        "commit",
        "-qm",
        "baseline",
    ]);
    run_git(&["checkout", "-q", "-b", "side"]);
    std::fs::write(tmp.path().join("tracked.txt"), "side\n")
        .expect("the test fixture file must be writable");
    run_git(&[
        "-c",
        "user.name=Peri Test",
        "-c",
        "user.email=peri-test@example.invalid",
        "commit",
        "-qam",
        "side",
    ]);
    run_git(&["checkout", "-q", "-"]);
    std::fs::write(tmp.path().join("tracked.txt"), "main\n")
        .expect("the test fixture file must be writable");
    run_git(&[
        "-c",
        "user.name=Peri Test",
        "-c",
        "user.email=peri-test@example.invalid",
        "commit",
        "-qam",
        "main",
    ]);
    let merge = std::process::Command::new("git")
        .args([
            "-c",
            "user.name=Peri Test",
            "-c",
            "user.email=peri-test@example.invalid",
            "merge",
            "--no-ff",
            "side",
        ])
        .current_dir(tmp.path())
        .status()
        .expect("the merge fixture result is required for this assertion");
    assert!(!merge.success(), "fixture 必须进入未合并状态");
    let status = std::process::Command::new("git")
        .args(["status", "--porcelain=v2", "-z", "--untracked-files=all"])
        .current_dir(tmp.path())
        .output()
        .expect("the status fixture result is required for this assertion");
    let status =
        String::from_utf8(status.stdout).expect("the status fixture output must be valid UTF-8");
    assert!(
        status.split('\0').any(|field| field.starts_with("u ")),
        "fixture 必须产生未合并记录: {status}"
    );

    let baseline = GitBaseline::capture(tmp.path())
        .expect("the capture fixture result is required for this assertion");
    baseline
        .verify_unchanged()
        .expect("未合并条目本身不是运行带来的变化");
}

#[test]
fn workflow_journal_is_excluded_but_unrelated_untracked_files_are_not() {
    let tmp = TempDir::new().expect("the new fixture result is required for this assertion");
    let run_git = |args: &[&str]| {
        assert!(std::process::Command::new("git")
            .args(args)
            .current_dir(tmp.path())
            .status()
            .expect("the status fixture result is required for this assertion")
            .success());
    };
    run_git(&["init", "-q"]);
    std::fs::write(tmp.path().join("tracked.txt"), "base\n")
        .expect("the test fixture file must be writable");
    // 刻意不忽略 `.claude/`：宿主自己的运行产物不得被当成外部变化。
    run_git(&["add", "tracked.txt"]);
    run_git(&[
        "-c",
        "user.name=Peri Test",
        "-c",
        "user.email=peri-test@example.invalid",
        "commit",
        "-qm",
        "baseline",
    ]);

    let baseline = GitBaseline::capture(tmp.path())
        .expect("the capture fixture result is required for this assertion");
    let run_dir = tmp
        .path()
        .join(".claude")
        .join("workflow-runs")
        .join("run-1");
    std::fs::create_dir_all(&run_dir).expect("the run directory must be creatable");
    std::fs::write(run_dir.join("script.js"), "return null")
        .expect("the test fixture file must be writable");
    baseline
        .verify_unchanged()
        .expect("本 run 自己的 journal 不构成外部变化");

    std::fs::write(tmp.path().join("notes.txt"), "external\n")
        .expect("the test fixture file must be writable");
    let failure = baseline
        .verify_unchanged()
        .expect_err("journal 之外的未跟踪文件仍必须报告");
    assert!(
        matches!(
            failure,
            crate::journal::PostconditionFailure::Unattributed(_)
        ),
        "无 write footprint 时变化只能判为未归因: {failure}"
    );
    assert!(failure.to_string().contains("notes.txt"));
}

/// rename 只有两端都在 workflow journal 内才是宿主产物：从允许范围外 rename
/// 进 journal 时，源路径的消失仍必须报告。
#[test]
fn rename_into_the_workflow_journal_does_not_hide_the_missing_source() {
    let tmp = TempDir::new().expect("the new fixture result is required for this assertion");
    let run_git = |args: &[&str]| {
        assert!(std::process::Command::new("git")
            .args(args)
            .current_dir(tmp.path())
            .status()
            .expect("the status fixture result is required for this assertion")
            .success());
    };
    run_git(&["init", "-q"]);
    std::fs::write(tmp.path().join("tracked.txt"), "base\n")
        .expect("the test fixture file must be writable");
    run_git(&["add", "tracked.txt"]);
    run_git(&[
        "-c",
        "user.name=Peri Test",
        "-c",
        "user.email=peri-test@example.invalid",
        "commit",
        "-qm",
        "baseline",
    ]);
    let canonical = tmp
        .path()
        .canonicalize()
        .expect("the temporary directory must be canonicalizable");
    let intent = peri_acp_types::workflow::WorkflowWriteIntent::Write {
        repo_root: canonical.to_string_lossy().into_owned(),
        cwd: canonical.to_string_lossy().into_owned(),
        path_allowlist: vec!["src".into()],
        head_may_change: false,
        commit_required: None,
    };
    let baseline = GitBaseline::capture(tmp.path())
        .expect("the capture fixture result is required for this assertion");

    let run_dir = tmp.path().join(".claude/workflow-runs/run-1");
    std::fs::create_dir_all(&run_dir).expect("the run directory must be creatable");
    run_git(&[
        "mv",
        "tracked.txt",
        ".claude/workflow-runs/run-1/tracked.txt",
    ]);

    let failure = baseline
        .verify_postcondition(Some(&intent))
        .expect_err("allowlist 外的源文件消失必须报告");
    assert!(
        failure.is_violation(),
        "allowlist 越界是可归因违例: {failure}"
    );
    assert!(
        failure.to_string().contains("tracked.txt"),
        "报告必须带源路径: {failure}"
    );
}

/// 非 ASCII 工作目录下 journal 路径也必须被识别：`-z` 避免 git 的 C-quote。
#[test]
fn workflow_journal_is_excluded_under_a_non_ascii_nested_cwd() {
    let tmp = TempDir::new().expect("the new fixture result is required for this assertion");
    let nested = tmp.path().join("子目录");
    std::fs::create_dir_all(&nested).expect("the nested working directory must be creatable");
    let run_git = |args: &[&str]| {
        assert!(std::process::Command::new("git")
            .args(args)
            .current_dir(tmp.path())
            .status()
            .expect("the status fixture result is required for this assertion")
            .success());
    };
    run_git(&["init", "-q"]);
    std::fs::write(tmp.path().join("tracked.txt"), "base\n")
        .expect("the test fixture file must be writable");
    run_git(&["add", "tracked.txt"]);
    run_git(&[
        "-c",
        "user.name=Peri Test",
        "-c",
        "user.email=peri-test@example.invalid",
        "commit",
        "-qm",
        "baseline",
    ]);
    let baseline = GitBaseline::capture(&nested)
        .expect("the capture fixture result is required for this assertion");

    let run_dir = nested.join(".claude/workflow-runs/run-1");
    std::fs::create_dir_all(&run_dir).expect("the run directory must be creatable");
    std::fs::write(run_dir.join("script.js"), "return null")
        .expect("the test fixture file must be writable");
    baseline
        .verify_unchanged()
        .expect("非 ASCII cwd 下的自身 journal 不构成外部变化");
}

/// 两个不同的非 UTF-8 路径若有损解码会折叠成同一个键，从而漏掉变化（false green）。
/// rename 的原始路径是独立字段，解析层必须把两端都留成各自的字节键。
#[test]
fn parse_keeps_distinct_non_utf8_paths_and_rename_endpoints() {
    let untracked = b"? swap-\xff.bin\0? swap-\xfe.bin\0";
    let records = crate::journal::git::parse_status_paths(untracked)
        .expect("non-UTF-8 status must remain parseable");
    assert_eq!(
        records.len(),
        2,
        "distinct byte paths must not collapse into one key"
    );
    assert!(records.contains_key(b"swap-\xff.bin".as_slice()));
    assert!(records.contains_key(b"swap-\xfe.bin".as_slice()));

    let renamed = b"2 R. N... 100644 100644 100644 aaaa bbbb R100 new-\xff.txt\0old-\xfe.txt\0";
    let records = crate::journal::git::parse_status_paths(renamed)
        .expect("rename records must remain parseable");
    assert_eq!(records.len(), 2, "rename source must be its own record key");
    for endpoint in [b"new-\xff.txt".as_slice(), b"old-\xfe.txt".as_slice()] {
        let record = records
            .get(endpoint)
            .unwrap_or_else(|| panic!("missing rename endpoint {endpoint:?}"));
        assert_eq!(
            record.first(),
            Some(&b'2'),
            "rename record type must survive"
        );
    }
}

/// 采到的 status 含非 UTF-8 路径时，对账必须按字节工作，不能退化成
/// observation incomplete；报告中的路径允许有损展示。
#[test]
fn non_utf8_status_path_does_not_become_an_incomplete_observation() {
    let tmp = TempDir::new().expect("the new fixture result is required for this assertion");
    let run_git = |args: &[&str]| {
        assert!(std::process::Command::new("git")
            .args(args)
            .current_dir(tmp.path())
            .status()
            .expect("the status fixture result is required for this assertion")
            .success());
    };
    run_git(&["init", "-q"]);
    std::fs::write(tmp.path().join("tracked.txt"), "base\n")
        .expect("the test fixture file must be writable");
    run_git(&["add", "tracked.txt"]);
    run_git(&[
        "-c",
        "user.name=Peri Test",
        "-c",
        "user.email=peri-test@example.invalid",
        "commit",
        "-qm",
        "baseline",
    ]);
    let mut baseline = GitBaseline::capture(tmp.path())
        .expect("the capture fixture result is required for this assertion");
    // macOS APFS 拒绝创建非 UTF-8 文件名，这里直接注入等价的 porcelain 记录。
    baseline.status_porcelain_v2 = b"? dirty-\xff.txt\0".to_vec();

    let failure = baseline
        .verify_unchanged()
        .expect_err("注入的未跟踪记录是真实差异");
    assert!(
        matches!(
            failure,
            crate::journal::PostconditionFailure::Unattributed(_)
        ),
        "非 UTF-8 路径不得被读成 observation incomplete: {failure}"
    );
    let message = failure.to_string();
    assert!(
        message.contains("dirty-"),
        "报告必须带可定位路径（展示可有损）: {message}"
    );
}

/// 真实文件系统上的非 UTF-8 文件名：采样不失败，两个不同路径的 rename 两端都能被区分。
/// macOS 的 APFS 拒绝此类文件名（EILSEQ），因此只在 Linux 验证。
#[cfg(target_os = "linux")]
#[test]
fn non_utf8_paths_are_sampled_and_reconciled_by_bytes() {
    use std::os::unix::ffi::OsStrExt;

    let tmp = TempDir::new().expect("the new fixture result is required for this assertion");
    let run_git = |args: &[&str]| {
        assert!(std::process::Command::new("git")
            .args(args)
            .current_dir(tmp.path())
            .status()
            .expect("the status fixture result is required for this assertion")
            .success());
    };
    run_git(&["init", "-q"]);
    std::fs::write(tmp.path().join("tracked.txt"), "base\n")
        .expect("the test fixture file must be writable");
    run_git(&["add", "tracked.txt"]);
    run_git(&[
        "-c",
        "user.name=Peri Test",
        "-c",
        "user.email=peri-test@example.invalid",
        "commit",
        "-qm",
        "baseline",
    ]);
    let first = std::ffi::OsStr::from_bytes(b"swap-\xff.bin");
    std::fs::write(tmp.path().join(first), b"payload")
        .expect("the test fixture file must be writable");
    let baseline = GitBaseline::capture(tmp.path())
        .expect("the capture fixture result is required for this assertion");
    baseline
        .verify_unchanged()
        .expect("非 UTF-8 路径属于基线，既不是变化也不是观测不完整");

    let second = std::ffi::OsStr::from_bytes(b"swap-\xfe.bin");
    std::fs::rename(tmp.path().join(first), tmp.path().join(second))
        .expect("the test fixture must be renamable");
    let failure = baseline
        .verify_unchanged()
        .expect_err("两个不同非 UTF-8 路径之间的 rename 必须被观察到");
    assert!(
        matches!(
            failure,
            crate::journal::PostconditionFailure::Unattributed(_)
        ),
        "非 UTF-8 路径不得被读成 observation incomplete: {failure}"
    );
    let message = failure.to_string();
    assert_eq!(
        message.matches("swap-").count(),
        2,
        "rename 的两端必须各自作为独立字节路径参与对账: {message}"
    );
}

/// 符号链接指向的同一目录必须得到同一分类：损坏的仓库是“无法观测”，
/// 不能因为按字面路径找不到 `.git` marker 而读成“没有仓库范围”。
#[cfg(unix)]
#[test]
fn broken_repository_behind_a_symlink_is_an_incomplete_observation() {
    let tmp = TempDir::new().expect("the temporary test directory must be available");
    let repo = tmp.path().join("repo");
    let sub = repo.join("sub");
    std::fs::create_dir_all(&sub).expect("the test fixture directory must be creatable");
    std::fs::write(repo.join(".git"), "gitdir: /nonexistent/peri-issue-522\n")
        .expect("the test fixture file must be writable");
    let alias = tmp.path().join("alias");
    std::os::unix::fs::symlink(&sub, &alias).expect("the test fixture symlink must be creatable");

    let observation = GitBaseline::capture_for_intent(&alias, None)
        .expect("sampling failure must not fail start");
    assert!(
        matches!(observation, GitObservation::Incomplete(_)),
        "同一目录经符号链接访问仍是采样失败: {observation:?}"
    );
}

#[test]
fn test_write_postcondition_ignores_unchanged_preexisting_dirty_path() {
    let tmp = TempDir::new().expect("the new fixture result is required for this assertion");
    let run_git = |args: &[&str]| {
        assert!(std::process::Command::new("git")
            .args(args)
            .current_dir(tmp.path())
            .status()
            .expect("the status fixture result is required for this assertion")
            .success());
    };
    run_git(&["init", "-q"]);
    std::fs::write(tmp.path().join("allowed.txt"), "base\n")
        .expect("the test fixture file must be writable");
    std::fs::write(tmp.path().join("outside.txt"), "base\n")
        .expect("the test fixture file must be writable");
    run_git(&["add", "."]);
    run_git(&[
        "-c",
        "user.name=Peri Test",
        "-c",
        "user.email=peri-test@example.invalid",
        "commit",
        "-qm",
        "baseline",
    ]);
    std::fs::write(tmp.path().join("outside.txt"), "pre-existing dirty\n")
        .expect("the test fixture file must be writable");

    let baseline = GitBaseline::capture(tmp.path())
        .expect("the capture fixture result is required for this assertion");
    let intent = peri_acp_types::workflow::WorkflowWriteIntent::Write {
        repo_root: tmp.path().to_string_lossy().into_owned(),
        cwd: tmp.path().to_string_lossy().into_owned(),
        path_allowlist: vec!["allowed.txt".into()],
        head_may_change: false,
        commit_required: None,
    };
    std::fs::write(tmp.path().join("allowed.txt"), "changed\n")
        .expect("the test fixture file must be writable");

    baseline
        .verify_postcondition(Some(&intent))
        .expect("the verify postcondition fixture result is required for this assertion");
}

#[test]
fn test_write_postcondition_allows_only_allowlisted_change() {
    let tmp = TempDir::new().expect("the new fixture result is required for this assertion");
    let run_git = |args: &[&str]| {
        assert!(std::process::Command::new("git")
            .args(args)
            .current_dir(tmp.path())
            .status()
            .expect("the status fixture result is required for this assertion")
            .success());
    };
    run_git(&["init", "-q"]);
    std::fs::write(tmp.path().join("allowed.txt"), "base\n")
        .expect("the test fixture file must be writable");
    std::fs::write(tmp.path().join("outside.txt"), "base\n")
        .expect("the test fixture file must be writable");
    run_git(&["add", "."]);
    run_git(&[
        "-c",
        "user.name=Peri Test",
        "-c",
        "user.email=peri-test@example.invalid",
        "commit",
        "-qm",
        "baseline",
    ]);

    let baseline = GitBaseline::capture(tmp.path())
        .expect("the capture fixture result is required for this assertion");
    let intent = peri_acp_types::workflow::WorkflowWriteIntent::Write {
        repo_root: tmp.path().to_string_lossy().into_owned(),
        cwd: tmp.path().to_string_lossy().into_owned(),
        path_allowlist: vec!["allowed.txt".into()],
        head_may_change: false,
        commit_required: None,
    };
    baseline
        .validate_write_intent(&intent)
        .expect("the validate write intent fixture result is required for this assertion");
    std::fs::write(tmp.path().join("allowed.txt"), "changed\n")
        .expect("the test fixture file must be writable");
    baseline
        .verify_postcondition(Some(&intent))
        .expect("the verify postcondition fixture result is required for this assertion");
    std::fs::write(tmp.path().join("outside.txt"), "escaped\n")
        .expect("the test fixture file must be writable");
    let error = baseline.verify_postcondition(Some(&intent)).unwrap_err();
    assert!(error.is_violation(), "allowlist 越界必须是可归因违例");
    let message = error.to_string();
    assert!(message.contains("escaped"));
    assert!(
        message.contains("outside.txt"),
        "越界报告必须带路径: {message}"
    );
}

#[test]
fn test_write_intent_rejects_parent_traversal() {
    let tmp = TempDir::new().expect("the new fixture result is required for this assertion");
    assert!(std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(tmp.path())
        .status()
        .expect("the status fixture result is required for this assertion")
        .success());
    std::fs::write(tmp.path().join("tracked.txt"), "base\n")
        .expect("the test fixture file must be writable");
    assert!(std::process::Command::new("git")
        .args(["add", "tracked.txt"])
        .current_dir(tmp.path())
        .status()
        .expect("the status fixture result is required for this assertion")
        .success());
    assert!(std::process::Command::new("git")
        .args([
            "-c",
            "user.name=Peri Test",
            "-c",
            "user.email=peri-test@example.invalid",
            "commit",
            "-qm",
            "baseline"
        ])
        .current_dir(tmp.path())
        .status()
        .expect("the status fixture result is required for this assertion")
        .success());
    let baseline = GitBaseline::capture(tmp.path())
        .expect("the capture fixture result is required for this assertion");
    let intent = peri_acp_types::workflow::WorkflowWriteIntent::Write {
        repo_root: tmp.path().to_string_lossy().into_owned(),
        cwd: tmp.path().to_string_lossy().into_owned(),
        path_allowlist: vec!["../outside".into()],
        head_may_change: false,
        commit_required: None,
    };
    assert!(baseline.validate_write_intent(&intent).is_err());
}

// ─── extract_long_texts / write_output 测试 ────────────────────

/// [回归测试] label 归一：分隔符/控制字符替换、连续点折叠、空 label 回落、
/// 重名去重（同一 label 不得覆盖）。
#[test]
fn test_output_label_sanitizes_and_dedups() {
    let mut used = std::collections::HashSet::new();
    assert_eq!(
        crate::journal::output::output_label("a.b[0]", &mut used),
        "a.b[0]"
    );
    assert_eq!(
        crate::journal::output::output_label("raw/crates", &mut used),
        "raw_crates"
    );
    assert_eq!(
        crate::journal::output::output_label("raw\\crates", &mut used),
        "raw_crates-2"
    );
    assert_eq!(crate::journal::output::output_label("..", &mut used), "._");
    assert_eq!(
        crate::journal::output::output_label("", &mut used),
        "unnamed"
    );
}

/// [回归测试] 占位符与文件名同源：label 含 `/`（非法路径字符）时，state.json 写入的
/// `${label}` 必须指向真实存在的 `outputs/<label>.txt`；两个会归一到同一名字的
/// label 不得互相覆盖（2026-10-11 issue：引用断裂 / 内容被覆盖）。
#[test]
fn test_extract_long_texts_label_matches_output_file() {
    let (_tmp, store) = make_store();
    store
        .init_run("r1", "script")
        .expect("the init run fixture result is required for this assertion");
    let first = "a".repeat(250);
    let second = "b".repeat(260);
    let mut value = serde_json::json!({"raw/crates": first.clone(), "raw_crates": second.clone()});
    let extracted = crate::journal::output::extract_long_texts(&mut value, "r1", &store, 200);
    let mut labels = extracted.clone();
    labels.sort();
    assert_eq!(labels, vec!["raw_crates", "raw_crates-2"]);

    // 每个占位符都必须解析到真实文件，两份内容都不丢。
    let outputs = store.run_dir("r1").join("outputs");
    let mut resolved: Vec<(String, String)> = ["raw/crates", "raw_crates"]
        .into_iter()
        .map(|key| {
            let placeholder = value[key].as_str().expect("长文本应替换为占位符");
            let name = placeholder
                .strip_prefix("${")
                .and_then(|rest| rest.strip_suffix('}'))
                .expect("占位符形态必须为 ${label}");
            let content = std::fs::read_to_string(outputs.join(format!("{name}.txt")))
                .unwrap_or_else(|error| panic!("占位符 {placeholder} 未指向真实文件: {error}"));
            (name.to_string(), content)
        })
        .collect();
    resolved.sort();
    assert_eq!(
        resolved,
        vec![
            ("raw_crates".to_string(), first),
            ("raw_crates-2".to_string(), second),
        ]
    );
}

#[test]
fn test_extract_long_texts_empty() {
    let (_tmp, store) = make_store();
    store
        .init_run("r1", "script")
        .expect("the init run fixture result is required for this assertion");
    let mut value = serde_json::json!({});
    let extracted = crate::journal::output::extract_long_texts(&mut value, "r1", &store, 200);
    assert!(extracted.is_empty());
    assert_eq!(value, serde_json::json!({}));
}

#[test]
fn test_extract_long_texts_short_strings() {
    let (_tmp, store) = make_store();
    store
        .init_run("r1", "script")
        .expect("the init run fixture result is required for this assertion");
    let mut value = serde_json::json!({"k": "short"});
    let extracted = crate::journal::output::extract_long_texts(&mut value, "r1", &store, 200);
    assert!(extracted.is_empty());
    assert_eq!(value, serde_json::json!({"k": "short"}));
}

#[test]
fn test_extract_long_texts_top_level() {
    let (_tmp, store) = make_store();
    store
        .init_run("r1", "script")
        .expect("the init run fixture result is required for this assertion");
    let long_str = "a".repeat(250);
    let mut value = serde_json::json!({"result": long_str.clone()});
    let extracted = crate::journal::output::extract_long_texts(&mut value, "r1", &store, 200);
    assert_eq!(extracted.len(), 1);
    assert_eq!(extracted[0], "result");
    // 原位置替换为 ${label}
    assert_eq!(
        value["result"]
            .as_str()
            .expect("the as str fixture result is required for this assertion"),
        "${result}"
    );
    // 验证文件写入
    let content = std::fs::read_to_string(store.run_dir("r1").join("outputs").join("result.txt"))
        .expect("the test fixture text file must be readable");
    assert_eq!(content, long_str);
}

#[test]
fn test_extract_long_texts_array() {
    let (_tmp, store) = make_store();
    store
        .init_run("r1", "script")
        .expect("the init run fixture result is required for this assertion");
    let long1 = "b".repeat(300);
    let long2 = "c".repeat(300);
    let mut value = serde_json::json!({"items": [
        {"result": long1.clone()},
        {"result": long2.clone()}
    ]});
    let extracted = crate::journal::output::extract_long_texts(&mut value, "r1", &store, 200);
    assert_eq!(extracted.len(), 2);
    // 使用索引标签，无覆盖
    assert_eq!(extracted[0], "items[0].result");
    assert_eq!(extracted[1], "items[1].result");
    // 原位置替换
    assert_eq!(
        value["items"][0]["result"]
            .as_str()
            .expect("the as str fixture result is required for this assertion"),
        "${items[0].result}"
    );
    assert_eq!(
        value["items"][1]["result"]
            .as_str()
            .expect("the as str fixture result is required for this assertion"),
        "${items[1].result}"
    );
    // 验证两个文件均正确写入，无覆盖
    let content0 = std::fs::read_to_string(
        store
            .run_dir("r1")
            .join("outputs")
            .join("items[0].result.txt"),
    )
    .expect("the test fixture text file must be readable");
    assert_eq!(content0, long1);
    let content1 = std::fs::read_to_string(
        store
            .run_dir("r1")
            .join("outputs")
            .join("items[1].result.txt"),
    )
    .expect("the test fixture text file must be readable");
    assert_eq!(content1, long2);
}

#[test]
fn test_extract_long_texts_nested() {
    let (_tmp, store) = make_store();
    store
        .init_run("r1", "script")
        .expect("the init run fixture result is required for this assertion");
    let inner_long = "d".repeat(250);
    let mut value = serde_json::json!({
        "outer": {
            "inner": inner_long.clone()
        }
    });
    let extracted = crate::journal::output::extract_long_texts(&mut value, "r1", &store, 200);
    assert_eq!(extracted.len(), 1);
    // 点号分隔标签
    assert_eq!(extracted[0], "outer.inner");
    assert_eq!(
        value["outer"]["inner"]
            .as_str()
            .expect("the as str fixture result is required for this assertion"),
        "${outer.inner}"
    );
    let content =
        std::fs::read_to_string(store.run_dir("r1").join("outputs").join("outer.inner.txt"))
            .expect("the test fixture text file must be readable");
    assert_eq!(content, inner_long);
}

/// [回归测试] 文件写失败时不能把仍未持久化的原文替换成不可读取的引用。
#[test]
fn test_extract_long_texts_preserves_content_when_output_write_fails() {
    let (_tmp, store) = make_store();
    store
        .init_run("failed-output", "script")
        .expect("the init run fixture result is required for this assertion");
    let outputs = store.run_dir("failed-output").join("outputs");
    std::fs::create_dir_all(outputs.join("blocked.txt"))
        .expect("the test fixture directory must be creatable");
    let original = "retained".repeat(40);
    let saved = "persisted".repeat(40);
    let mut value = serde_json::json!({"blocked": original, "saved": saved});
    let extracted =
        crate::journal::output::extract_long_texts(&mut value, "failed-output", &store, 200);
    assert_eq!(extracted, vec!["saved"]);
    assert_eq!(value["blocked"], original, "失败分支必须保留可用正文");
    assert_eq!(value["saved"], "${saved}");
    assert_eq!(
        std::fs::read_to_string(outputs.join("saved.txt"))
            .expect("the test fixture text file must be readable"),
        saved
    );
}

#[test]
fn attempt_tracker_makes_engine_retries_visible_and_preserves_recovery() {
    let mut tracker = AttemptTracker::default();
    tracker.record_dispatch("run-1", 7, "task-1");
    tracker.record_dispatch("run-1", 7, "task-2");
    // 同一 task 的重复通知不得重复计数。
    tracker.record_dispatch("run-1", 7, "task-2");
    let mut entry = JournalEntry {
        key: "key-7".into(),
        seq: 7,
        result: AgentRunResult::Skipped,
        attempt: Some(WorkflowAttempt {
            run_id: "run-1".into(),
            agent_id: None,
            journal_seq: 7,
            attempt_index: None,
            recovered_from: None,
            consumed: true,
            disposition: AttemptDisposition::Produced,
            execution: None,
        }),
    };
    tracker.annotate(&mut entry);
    let attempt = entry.attempt.as_ref().expect("attempt must remain present");
    assert_eq!(attempt.attempt_index, Some(2), "两次派发必须计为两次尝试");
    assert_eq!(
        attempt.agent_id,
        Some(7),
        "host 可证明的 agent identity 应回填"
    );

    let mut recovered = JournalEntry {
        key: "key-8".into(),
        seq: 8,
        result: AgentRunResult::Skipped,
        attempt: Some(WorkflowAttempt {
            run_id: "run-1".into(),
            agent_id: Some(8),
            journal_seq: 8,
            attempt_index: Some(3),
            recovered_from: None,
            consumed: true,
            disposition: AttemptDisposition::Recovered,
            execution: None,
        }),
    };
    tracker.annotate(&mut recovered);
    assert_eq!(
        recovered
            .attempt
            .as_ref()
            .and_then(|attempt| attempt.attempt_index),
        Some(3),
        "缓存恢复不得把旧尝试序号重置为本 run 的派发数"
    );
}

#[test]
fn no_write_intent_still_samples_git_baseline_inside_a_repository() {
    let tmp = TempDir::new().expect("the temporary test directory must be available");
    assert!(std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(tmp.path())
        .status()
        .expect("the status fixture result is required for this assertion")
        .success());
    std::fs::write(tmp.path().join("tracked.txt"), "base\n")
        .expect("the test fixture file must be writable");
    assert!(std::process::Command::new("git")
        .args(["add", "tracked.txt"])
        .current_dir(tmp.path())
        .status()
        .expect("the status fixture result is required for this assertion")
        .success());
    assert!(std::process::Command::new("git")
        .args([
            "-c",
            "user.name=Peri Test",
            "-c",
            "user.email=peri-test@example.invalid",
            "commit",
            "-qm",
            "baseline",
        ])
        .current_dir(tmp.path())
        .status()
        .expect("the status fixture result is required for this assertion")
        .success());
    let canonical = tmp
        .path()
        .canonicalize()
        .expect("the temporary directory must be canonicalizable");

    for intent in [
        None,
        Some(&peri_acp_types::workflow::WorkflowWriteIntent::ReadOnly),
    ] {
        let observation = GitBaseline::capture_for_intent(tmp.path(), intent)
            .expect("sampling without a write intent must not fail start");
        match observation {
            GitObservation::Captured(baseline) => {
                assert_eq!(baseline.repo_root, canonical);
                assert_eq!(baseline.cwd, canonical);
            }
            other => panic!("inside a repository the baseline must be sampled: {other:?}"),
        }
    }
}

#[test]
fn broken_git_repository_is_incomplete_not_not_applicable() {
    let tmp = TempDir::new().expect("the temporary test directory must be available");
    std::fs::write(
        tmp.path().join(".git"),
        "gitdir: /nonexistent/peri-issue-522\n",
    )
    .expect("the test fixture file must be writable");

    let observation = GitBaseline::capture_for_intent(tmp.path(), None)
        .expect("sampling failure must not fail start");
    assert!(
        observation.scope().is_none(),
        "采样失败没有可持久化的观测范围"
    );
    assert!(
        matches!(observation, GitObservation::Incomplete(_)),
        "损坏的仓库是采样失败，不得读作没有可观测范围: {observation:?}"
    );
}

#[test]
fn git_baseline_records_repository_identity_and_visible_scope() {
    let tmp = TempDir::new().expect("the temporary test directory must be available");
    let run_git = |args: &[&str]| {
        assert!(std::process::Command::new("git")
            .args(args)
            .current_dir(tmp.path())
            .status()
            .expect("the status fixture result is required for this assertion")
            .success());
    };
    run_git(&["init", "-q"]);
    std::fs::write(tmp.path().join("tracked.txt"), "base\n")
        .expect("the test fixture file must be writable");
    run_git(&["add", "tracked.txt"]);
    run_git(&[
        "-c",
        "user.name=Peri Test",
        "-c",
        "user.email=peri-test@example.invalid",
        "commit",
        "-qm",
        "baseline",
    ]);
    let canonical = tmp
        .path()
        .canonicalize()
        .expect("the temporary directory must be canonicalizable");

    let baseline = GitBaseline::capture(tmp.path())
        .expect("the capture fixture result is required for this assertion");
    assert_eq!(baseline.git_dir, canonical.join(".git"));
    assert_eq!(baseline.git_common_dir, canonical.join(".git"));

    let scope = baseline.observation_scope();
    assert_eq!(scope.repo_root, canonical.to_string_lossy());
    assert_eq!(scope.cwd, canonical.to_string_lossy());
    assert_eq!(scope.git_dir, canonical.join(".git").to_string_lossy());
    assert_eq!(scope.head, baseline.head);
    assert!(scope.visibility.index);
    assert!(scope.visibility.worktree);
    assert!(
        !scope.visibility.ignored,
        "ignored 文件不进入基线，其变化不可观测"
    );
    assert!(
        !scope.visibility.commit_range,
        "只比较运行前后 HEAD 端点，不覆盖完整提交区间"
    );
}

#[test]
fn postcondition_failure_messages_carry_the_attribution_tier() {
    use crate::journal::PostconditionFailure;

    for (failure, message, display) in [
        (
            PostconditionFailure::Violation("escaped writeIntent.path_allowlist".into()),
            "escaped writeIntent.path_allowlist",
            "run-owned violation: escaped writeIntent.path_allowlist",
        ),
        (
            PostconditionFailure::Unattributed("git HEAD changed".into()),
            "git HEAD changed",
            "unattributed change (external or unknown): git HEAD changed",
        ),
        (
            PostconditionFailure::ObservationIncomplete("cannot sample".into()),
            "cannot sample",
            "observation incomplete: cannot sample",
        ),
    ] {
        assert_eq!(failure.to_string(), display);
        assert_eq!(
            failure.message(),
            message,
            "结构化 message 保持不带档位前缀"
        );
    }
}

/// 观测窗口被重新绑定（cwd 指向另一个 git dir）时不能对比旧基线宣称“无变化”。
#[cfg(unix)]
#[test]
fn rebinding_the_git_directory_is_incomplete_not_an_unattributed_change() {
    use crate::journal::PostconditionFailure;

    let tmp = TempDir::new().expect("the temporary test directory must be available");
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).expect("the test fixture directory must be creatable");
    let run_git = |args: &[&str]| {
        assert!(std::process::Command::new("git")
            .args(args)
            .current_dir(&repo)
            .status()
            .expect("the status fixture result is required for this assertion")
            .success());
    };
    run_git(&["init", "-q"]);
    std::fs::write(repo.join("tracked.txt"), "base\n")
        .expect("the test fixture file must be writable");
    run_git(&["add", "tracked.txt"]);
    run_git(&[
        "-c",
        "user.name=Peri Test",
        "-c",
        "user.email=peri-test@example.invalid",
        "commit",
        "-qm",
        "baseline",
    ]);
    let baseline = GitBaseline::capture(&repo)
        .expect("the capture fixture result is required for this assertion");

    let moved_git = tmp.path().join("moved-git");
    std::fs::rename(repo.join(".git"), &moved_git)
        .expect("the test fixture git directory must be movable");
    std::os::unix::fs::symlink(&moved_git, repo.join(".git"))
        .expect("the test fixture symlink must be creatable");

    let failure = baseline
        .verify_unchanged()
        .expect_err("重新绑定 git dir 后不得宣称无变化");
    assert!(
        matches!(failure, PostconditionFailure::ObservationIncomplete(_)),
        "观测窗口切换属于无法观测，不是外部变化: {failure}"
    );
    assert!(failure.to_string().starts_with("observation incomplete: "));
}

/// 旧 `state.json` 没有限额明细：缺省为空/None，不得据此推断“未触发过限额”。
#[test]
fn test_legacy_state_without_limit_breaches_deserializes_as_unknown() {
    let state: RunState = serde_json::from_value(serde_json::json!({
        "run_id": "legacy-limits",
        "workflow_name": "legacy",
        "status": "failed",
        "script": "script",
        "started_at": "2026-11-21T00:00:00Z",
        "error": "workflow exceeded maxToolCalls (5)"
    }))
    .expect("from_value must accept this test fixture representation");

    assert!(state.limit_breaches.is_empty());
    assert_eq!(state.token_usage, None, "旧数据未采集用量，不显示为 0");
    assert_eq!(
        state.git_observation_scope, None,
        "旧数据未记录 Git 观测范围，不得推断为完整观测"
    );
}

/// 终态只保存观测身份与能力：camelCase 范围字段可读，基线内容（porcelain）不落盘。
#[test]
fn run_state_persists_git_observation_scope_without_baseline_content() {
    let state: RunState = serde_json::from_value(serde_json::json!({
        "run_id": "scope-run",
        "workflow_name": "scope",
        "status": "completed",
        "script": "script",
        "started_at": "2026-11-21T00:00:00Z",
        "git_observation_scope": {
            "repoRoot": "/repo",
            "cwd": "/repo/sub",
            "gitDir": "/repo/.git",
            "gitCommonDir": "/repo/.git",
            "head": "0123456789abcdef",
            "visibility": {
                "index": true,
                "worktree": true,
                "ignored": false,
                "commitRange": false
            }
        }
    }))
    .expect("from_value must accept git_observation_scope");

    let scope = state
        .git_observation_scope
        .as_ref()
        .expect("记录的范围必须可读");
    assert_eq!(scope.git_dir, "/repo/.git");
    assert_eq!(scope.git_common_dir, "/repo/.git");
    assert!(scope.visibility.index && scope.visibility.worktree);
    assert!(!scope.visibility.ignored && !scope.visibility.commit_range);

    let wire = serde_json::to_value(&state).expect("state must serialize");
    assert_eq!(wire["git_observation_scope"]["gitCommonDir"], "/repo/.git");
    assert!(
        wire.get("status_porcelain_v2").is_none()
            && wire["git_observation_scope"]
                .get("status_porcelain_v2")
                .is_none(),
        "终态不得持久化 baseline 内容"
    );
}

/// 新 state.json 的限额条目按 camelCase wire 写出，未采集字段缺席。
#[test]
fn test_limit_breaches_and_token_usage_serialize_without_zero_fillers() {
    let state = RunState {
        run_id: "run-limits".into(),
        workflow_name: "limits".into(),
        status: "failed".into(),
        execution_status: Default::default(),
        acceptance_status: Default::default(),
        post_processing_status: Default::default(),
        delivery_status: Default::default(),
        delivery_reason: None,
        postcondition_observation: None,
        git_observation_scope: None,
        outcome: Default::default(),
        write_intent: None,
        limits: crate::protocol::WorkflowLimits {
            max_tool_calls: Some(5),
            ..Default::default()
        },
        limit_breaches: vec![peri_acp_types::workflow::LimitBreach {
            scope: peri_acp_types::workflow::LimitScope::Iterations,
            limit: Some(400),
            observed: None,
            attempt: Some(peri_acp_types::workflow::LimitBreachAttempt {
                run_id: "run-limits".into(),
                task_id: Some("task-1".into()),
                agent_id: 3,
            }),
            detected_at: "2026-11-21T00:00:00Z".into(),
        }],
        token_usage: Some(75_323),
        budget_total: Some(60_000),
        args: None,
        max_concurrency: 3,
        attempts: Vec::new(),
        return_value: None,
        script: "script".into(),
        started_at: "2026-11-21T00:00:00Z".into(),
        finished_at: None,
        error: Some("workflow exceeded maxIterations (400)".into()),
    };
    let wire = serde_json::to_value(&state).expect("state must serialize");
    assert_eq!(wire["limits"]["maxToolCalls"], 5);
    assert_eq!(wire["limit_breaches"][0]["scope"], "iterations");
    assert_eq!(wire["limit_breaches"][0]["limit"], 400);
    assert!(
        wire["limit_breaches"][0].get("observed").is_none(),
        "未采集用量不得写成 0"
    );
    assert_eq!(wire["token_usage"], 75_323);
    assert_eq!(wire["budget_total"], 60_000);
}
