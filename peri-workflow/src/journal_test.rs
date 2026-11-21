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
        outcome: peri_acp_types::workflow::WorkflowOutcome::Blocked,
        write_intent: None,
        limits: crate::protocol::WorkflowLimits::default(),
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
        outcome: peri_acp_types::workflow::WorkflowOutcome::Blocked,
        write_intent: None,
        limits: crate::protocol::WorkflowLimits::default(),
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
        outcome: peri_acp_types::workflow::WorkflowOutcome::Completed,
        write_intent: None,
        limits: crate::protocol::WorkflowLimits::default(),
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
    assert!(error.contains("git pre/postcondition command failed"));
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
        matches!(observation, GitObservation::Incomplete(_)),
        "损坏的仓库是采样失败，不得读作没有可观测范围: {observation:?}"
    );
}
