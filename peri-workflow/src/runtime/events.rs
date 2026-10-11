//! run 事件日志与进度观察者：`workflow://runs/{runId}/events` 的事实源。
//!
//! 进度事件经 [`crate::progress::ProgressObserver`] 钩子到达本模块（store 提交后、
//! 锁外回调）：追加事件日志并发订阅唤醒。通知只是唤醒信号，正文一律经
//! `resources/read` 补读。
//!
//! [`WorkflowRunEventLog`] 在事件日志之上维护 **per-run 已提交水位**
//! （`workflow://runs/{runId}` 的 `eventWatermark`）：水位只在追加提交点前进，
//! 保留窗口淘汰不改写水位——已提交事实不因可读窗口收缩而回退（design §4.3 第 4 条；
//! 对抗审查 F4）。

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;
use serde::Serialize;
use serde_json::Value;
use tokio::sync::broadcast;

use crate::progress::ProgressObserver;
use crate::protocol::ProgressEvent;

use super::event_log::{BoundedEventLog, EventLogPage};
use super::WorkflowResourceUpdate;

/// run 事件类型（`ProgressEvent` 的粗分类，便于过滤与展示）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowRunEventKind {
    RunStarted,
    PhaseStarted,
    PhaseDone,
    AgentStarted,
    AgentProgress,
    AgentDone,
    Log,
    RunDone,
}

impl WorkflowRunEventKind {
    fn of(event: &ProgressEvent) -> Self {
        match event {
            ProgressEvent::RunStarted { .. } => Self::RunStarted,
            ProgressEvent::PhaseStarted { .. } => Self::PhaseStarted,
            ProgressEvent::PhaseDone { .. } => Self::PhaseDone,
            ProgressEvent::AgentStarted { .. } => Self::AgentStarted,
            ProgressEvent::AgentProgress { .. } => Self::AgentProgress,
            ProgressEvent::AgentDone { .. } => Self::AgentDone,
            ProgressEvent::Log { .. } => Self::Log,
            ProgressEvent::RunDone { .. } => Self::RunDone,
        }
    }
}

/// run 事件：追加不可变，单调 `sequence`（session 内跨 run 全局单调）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRunEvent {
    pub sequence: u64,
    pub run_id: String,
    pub kind: WorkflowRunEventKind,
    pub at_ms: i64,
    /// 事件正文（`ProgressEvent` 的 JSON 形态）。
    pub payload: Value,
}

/// run 事件日志：有界追加 + 永不倒退的 per-run 已提交水位。
///
/// 追加即提交：`sequence` 分配、正文入日志与水位前进在同一临界区内完成
/// （锁序恒为「水位 → 有界日志」，无反向路径）。读取方用
/// [`Self::committed_watermark`] 作为补读游标：水位单调不减，事件被淘汰后仍保持
/// 已提交值；可读缺口由事件页的 `truncatedBefore` 表达。
pub struct WorkflowRunEventLog {
    log: BoundedEventLog<WorkflowRunEvent>,
    watermarks: Mutex<HashMap<String, u64>>,
}

impl WorkflowRunEventLog {
    pub fn new(capacity: usize) -> Self {
        Self {
            log: BoundedEventLog::new(capacity),
            watermarks: Mutex::new(HashMap::new()),
        }
    }

    /// 追加一条事件（`sequence` 由日志分配；事件自带的 `sequence` 字段被忽略）。
    pub fn append(&self, event: WorkflowRunEvent) -> u64 {
        self.append_with(|_| event)
    }

    /// 追加一条事件：`make` 接收本次分配的 sequence 并构造值。
    pub fn append_with(&self, make: impl FnOnce(u64) -> WorkflowRunEvent) -> u64 {
        let mut watermarks = self.watermarks.lock();
        let mut appended_run: Option<String> = None;
        let sequence = self.log.append_with(|sequence| {
            let event = make(sequence);
            appended_run = Some(event.run_id.clone());
            event
        });
        if let Some(run_id) = appended_run {
            let watermark = watermarks.entry(run_id).or_insert(0);
            *watermark = (*watermark).max(sequence);
        }
        sequence
    }

    /// 该 run 的已提交水位（最后一条已提交事件的 sequence；无提交时为 0）。
    pub fn committed_watermark(&self, run_id: &str) -> u64 {
        self.watermarks.lock().get(run_id).copied().unwrap_or(0)
    }

    /// 带谓词的过滤分页（跨 run 聚合日志的单 run 视图）。
    pub fn page_where(
        &self,
        after: Option<u64>,
        limit: usize,
        predicate: impl Fn(&WorkflowRunEvent) -> bool,
    ) -> EventLogPage<WorkflowRunEvent> {
        self.log.page_where(after, limit, predicate)
    }
}

/// 进度观察者实现：记录 run 事件并发订阅唤醒。
pub(crate) struct RuntimeProgressObserver {
    events: Arc<WorkflowRunEventLog>,
    updates: broadcast::Sender<WorkflowResourceUpdate>,
}

impl RuntimeProgressObserver {
    pub(crate) fn new(
        events: Arc<WorkflowRunEventLog>,
        updates: broadcast::Sender<WorkflowResourceUpdate>,
    ) -> Self {
        Self { events, updates }
    }
}

impl ProgressObserver for RuntimeProgressObserver {
    fn on_progress_event(&self, event: &ProgressEvent) {
        let run_id = event.run_id().to_string();
        let kind = WorkflowRunEventKind::of(event);
        let payload = serde_json::to_value(event).unwrap_or(Value::Null);
        self.events.append_with(|sequence| WorkflowRunEvent {
            sequence,
            run_id: run_id.clone(),
            kind,
            at_ms: super::tasks::now_ms(),
            payload,
        });
        let _ = self.updates.send(WorkflowResourceUpdate::RunEvents {
            run_id: run_id.clone(),
        });
        let _ = self.updates.send(WorkflowResourceUpdate::Run {
            run_id: run_id.clone(),
        });
        // run 索引正文含状态与进度摘要：除纯日志外，每条进度事件提交后都要唤醒
        // 索引订阅者（审查 F5：索引不能只靠启动时一次补偿刷新）。
        if kind != WorkflowRunEventKind::Log {
            let _ = self.updates.send(WorkflowResourceUpdate::RunsIndex);
        }
    }

    fn on_runs_changed(&self) {
        // 清理淘汰改变了 `workflow://runs` 的正文（且被清理 run 的 scoped 资源
        // 转为 not-found）：提交后唤醒索引订阅者。
        let _ = self.updates.send(WorkflowResourceUpdate::RunsIndex);
    }
}
