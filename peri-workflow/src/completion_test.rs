//! 终态覆写失败必须可观测：读/写 `state.json` 失败时不得静默遗留
//! "磁盘 completed + 结果 failed"，且 progress 收敛不得依赖读成功。

use std::fmt::Write as _;
use std::sync::{Arc, Mutex, Weak};

use tracing::field::{Field, Visit};
use tracing::span;
use tracing::{Event, Metadata, Subscriber};

use super::RunCompletion;
use crate::journal::{RunState, WorkflowJournalStore};
use crate::progress::{RunStatus, WorkflowProgressStore};
use crate::protocol::{ProgressEvent, WorkflowLimits};
use peri_acp_types::workflow::{
    AcceptanceStatus, DeliveryStatus, ExecutionStatus, PostProcessingStatus,
};

// ─── 内联日志捕获（本文件专用，不引入 workspace 级测试工具）──────────
//
// `settle_failed_terminal_state` 在测试线程内同步执行，因此线程局部
// `set_default` 订阅即可捕获；不设级别过滤，由断言检查记录确为 WARN。

#[derive(Clone, Default)]
struct LogCapture(Arc<Mutex<String>>);

impl LogCapture {
    fn text(&self) -> String {
        self.0.lock().expect("log buffer poisoned").clone()
    }

    fn subscriber(&self) -> CaptureSubscriber {
        CaptureSubscriber {
            sink: Arc::clone(&self.0),
        }
    }
}

struct CaptureSubscriber {
    sink: Arc<Mutex<String>>,
}

struct RenderedFields(String);

impl Visit for RenderedFields {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if !self.0.is_empty() {
            self.0.push(' ');
        }
        let _ = write!(self.0, "{}={:?}", field.name(), value);
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if !self.0.is_empty() {
            self.0.push(' ');
        }
        let _ = write!(self.0, "{}={}", field.name(), value);
    }
}

impl Subscriber for CaptureSubscriber {
    fn enabled(&self, _metadata: &Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, _span: &span::Attributes<'_>) -> span::Id {
        span::Id::from_u64(1)
    }

    fn record(&self, _span: &span::Id, _values: &span::Record<'_>) {}

    fn record_follows_from(&self, _span: &span::Id, _follows: &span::Id) {}

    fn enter(&self, _span: &span::Id) {}

    fn exit(&self, _span: &span::Id) {}

    fn event(&self, event: &Event<'_>) {
        let mut fields = RenderedFields(String::new());
        event.record(&mut fields);
        let metadata = event.metadata();
        let mut sink = self.sink.lock().expect("log buffer poisoned");
        let _ = writeln!(
            sink,
            "{} target={} {}",
            metadata.level(),
            metadata.target(),
            fields.0
        );
    }
}

// ─── fixtures ─────────────────────────────────────────────

fn make_completion(
    journal: &Arc<WorkflowJournalStore>,
    progress: &Arc<WorkflowProgressStore>,
    run_id: &str,
) -> RunCompletion {
    RunCompletion {
        registry: Weak::new(),
        progress: Arc::clone(progress),
        journal: Arc::clone(journal),
        run_id: run_id.to_string(),
        name: "test-workflow".into(),
        started_at: std::time::Instant::now(),
    }
}

/// 预置一个仍处于 Running 的 run，用于断言收敛事件是否送达。
fn start_tracking(progress: &WorkflowProgressStore, run_id: &str) {
    progress.apply_event(&ProgressEvent::RunStarted {
        run_id: run_id.to_string(),
        workflow_name: "test-workflow".into(),
        meta: None,
    });
    assert!(matches!(
        progress.get_run(run_id).expect("run 应存在").status,
        RunStatus::Running
    ));
}

fn assert_failed_result(result: &crate::runner::WorkflowResult, error: &str) {
    assert_eq!(result.status, "failed");
    assert_eq!(result.error.as_deref(), Some(error));
    assert_eq!(result.delivery_status, DeliveryStatus::Blocked);
    assert_eq!(result.post_processing_status, PostProcessingStatus::Failed);
}

// ─── tests ────────────────────────────────────────────────

/// `read_state` 失败：日志必须声明磁盘态与结果可能不一致，且 progress
/// 收敛（RunDone{failed}）不得依赖读成功。
#[test]
fn read_state_failure_is_logged_and_progress_still_converges() {
    let tmp = tempfile::tempdir().unwrap();
    let journal = Arc::new(WorkflowJournalStore::new(tmp.path().to_str().unwrap()));
    let progress = Arc::new(WorkflowProgressStore::new());
    let run_id = "run-read-fail";
    start_tracking(&progress, run_id);
    // 未初始化 run 目录 → read_state 以 NotFound 失败
    assert!(journal.read_state(run_id).is_err());

    let completion = make_completion(&journal, &progress, run_id);
    let capture = LogCapture::default();
    let result = {
        let _guard = tracing::subscriber::set_default(capture.subscriber());
        completion.settle_failed_terminal_state("cleanup failed after success")
    };

    assert_failed_result(&result, "cleanup failed after success");

    let text = capture.text();
    assert!(
        text.contains("WARN"),
        "读失败必须在默认可见级别记录，实际: {text:?}"
    );
    assert!(text.contains(run_id), "日志必须含 run_id，实际: {text:?}");
    assert!(
        text.contains("failed to read workflow state"),
        "日志必须指明 state 读取失败，实际: {text:?}"
    );
    assert!(
        text.contains("may still report success"),
        "日志必须明确声明磁盘态与上报结果可能不一致，实际: {text:?}"
    );

    let run = progress.get_run(run_id).expect("run 应存在");
    assert!(
        matches!(run.status, RunStatus::Failed),
        "progress 不得停留 Running"
    );
    assert_eq!(run.delivery_status, DeliveryStatus::Blocked);
}

/// `write_state` 失败：`state.json` 确实停留在 completed（磁盘/结果不一致），
/// 但日志必须显式声明该不一致，且 progress 收敛。
#[test]
fn write_state_failure_is_logged_and_declares_stale_disk_state() {
    let tmp = tempfile::tempdir().unwrap();
    let journal = Arc::new(WorkflowJournalStore::new(tmp.path().to_str().unwrap()));
    let progress = Arc::new(WorkflowProgressStore::new());
    let run_id = "run-write-fail";
    start_tracking(&progress, run_id);

    journal.init_run(run_id, "return null").unwrap();
    let completed = RunState {
        run_id: run_id.to_string(),
        workflow_name: "test-workflow".into(),
        status: "completed".into(),
        execution_status: ExecutionStatus::Completed,
        acceptance_status: AcceptanceStatus::Unknown,
        post_processing_status: PostProcessingStatus::Blocked,
        delivery_status: DeliveryStatus::Blocked,
        write_intent: None,
        limits: WorkflowLimits::default(),
        budget_total: None,
        args: None,
        max_concurrency: 3,
        attempts: Vec::new(),
        return_value: None,
        script: "return null".into(),
        started_at: "2026-10-06T00:00:00Z".into(),
        finished_at: Some("2026-10-06T00:01:00Z".into()),
        error: None,
    };
    journal.write_state(run_id, &completed).unwrap();
    // state.json.tmp 被目录占用 → write_state 的 fs::write 失败，read_state 仍可用
    std::fs::create_dir(journal.run_dir(run_id).join("state.json.tmp")).unwrap();

    let completion = make_completion(&journal, &progress, run_id);
    let capture = LogCapture::default();
    let result = {
        let _guard = tracing::subscriber::set_default(capture.subscriber());
        completion.settle_failed_terminal_state("cleanup failed after success")
    };

    assert_failed_result(&result, "cleanup failed after success");
    assert_eq!(
        journal.read_state(run_id).unwrap().status,
        "completed",
        "测试前提：写失败后磁盘仍为 expired 成功态"
    );

    let text = capture.text();
    assert!(
        text.contains("WARN"),
        "写失败必须在默认可见级别记录，实际: {text:?}"
    );
    assert!(text.contains(run_id), "日志必须含 run_id，实际: {text:?}");
    assert!(
        text.contains("failed to persist failed terminal state"),
        "日志必须指明终态写失败，实际: {text:?}"
    );
    assert!(
        text.contains("while this run is reported as failed"),
        "日志必须明确声明磁盘态与上报结果不一致，实际: {text:?}"
    );

    let run = progress.get_run(run_id).expect("run 应存在");
    assert!(
        matches!(run.status, RunStatus::Failed),
        "progress 不得停留 Running"
    );
}
