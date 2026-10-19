//! workflow 实例的资源面：URI 解析、正文读取与分页。
//!
//! 资源是**不可信模型输入边界**：正文不经模型上下文注入；宿主（ACP/TUI 投影）
//! 经 `resources/read` 消费。事件日志读取使用稳定索引 URI + `?after=N&limit=M`，
//! `headSequence` / `truncatedBefore` 随页返回（design §4.3 协议）。
//!
//! run-specific 资源（`runs/{runId}`、`/events`、`/tasks`）的存在性以**同一**登记
//! 状态（progress store 的 run 记录）裁决：run 清理后三者同时 not-found。

use peri_workflow::runtime::{EventLogPage, TaskBaseline, WorkflowResourceUpdate, WorkflowRuntime};
use serde_json::{json, Value};

/// 固定资源 URI：run 索引快照。
pub const RUNS_URI: &str = "workflow://runs";
/// 固定资源 URI：session 级任务流（跨 run 聚合，消费主入口）。
pub const TASKS_URI: &str = "workflow://tasks";
/// 本实例资源 URI 的 scheme 前缀（订阅接受面的过滤条件）。
pub const WORKFLOW_URI_PREFIX: &str = "workflow://";

/// 单页读取默认条数。
pub const DEFAULT_PAGE_LIMIT: usize = 100;
/// 单页读取上界（有界读取，防一次拉爆）。
pub const MAX_PAGE_LIMIT: usize = 500;

const RUNS_PREFIX: &str = "workflow://runs/";

/// 已解析的资源地址（query 串已剥离）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkflowResource {
    Runs,
    BackgroundTasks,
    Run { run_id: String },
    RunEvents { run_id: String },
    RunTasks { run_id: String },
    Tasks,
}

/// 解析资源 URI；未登记的 scheme / 形状返回 `None`。
///
/// 只做形状解析，不校验 run 是否存在（存在性由读取正文时裁决）。
pub fn parse_uri(uri: &str) -> Option<WorkflowResource> {
    if uri == "workflow://background-tasks" {
        return Some(WorkflowResource::BackgroundTasks);
    }
    let path = uri.split('?').next().unwrap_or(uri);
    if path == RUNS_URI {
        return Some(WorkflowResource::Runs);
    }
    if path == TASKS_URI {
        return Some(WorkflowResource::Tasks);
    }
    let rest = path.strip_prefix(RUNS_PREFIX)?;
    let mut segments = rest.split('/');
    let run_id = segments.next().filter(|id| !id.is_empty())?;
    let run_id = run_id.to_string();
    match (segments.next(), segments.next()) {
        (None, None) => Some(WorkflowResource::Run { run_id }),
        (Some("events"), None) => Some(WorkflowResource::RunEvents { run_id }),
        (Some("tasks"), None) => Some(WorkflowResource::RunTasks { run_id }),
        _ => None,
    }
}

/// 分页参数（`?after={sequence}&limit=N`；缺省 `limit = DEFAULT_PAGE_LIMIT`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageQuery {
    pub after: Option<u64>,
    pub limit: usize,
}

pub fn parse_page_query(uri: &str) -> PageQuery {
    let mut query = PageQuery {
        after: None,
        limit: DEFAULT_PAGE_LIMIT,
    };
    if let Some((_, raw)) = uri.split_once('?') {
        for pair in raw.split('&') {
            let Some((key, value)) = pair.split_once('=') else {
                continue;
            };
            match key {
                "after" => query.after = value.parse::<u64>().ok(),
                "limit" => {
                    if let Ok(limit) = value.parse::<usize>() {
                        query.limit = limit.clamp(1, MAX_PAGE_LIMIT);
                    }
                }
                _ => {}
            }
        }
    }
    query
}

/// 资源读取失败（映射为 MCP 错误或 not-found 语义）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResourceReadError {
    /// 未登记的 URI 形状。
    NotSupported(String),
    /// 已登记的 run 不存在（未启动或已过保留窗口）。
    NotFound(String),
}

/// 读取资源正文（JSON 形态）。
///
/// 事件日志类资源的读取**不假装连续**：`after` 早于保留边界时从保留边界起读，
/// 页内的 `truncatedBefore` 让读取方识别游标失效并重建快照。
pub fn read_payload(runtime: &WorkflowRuntime, uri: &str) -> Result<Value, ResourceReadError> {
    let Some(resource) = parse_uri(uri) else {
        return Err(ResourceReadError::NotSupported(uri.to_string()));
    };
    match resource {
        WorkflowResource::BackgroundTasks => Ok(json!({
            "snapshot": runtime.bg_registry().map(|manager| manager.snapshot()),
            "completions": runtime.registry().pending_completions(),
        })),
        WorkflowResource::Runs => {
            Ok(serde_json::to_value(runtime.runs_snapshot()).unwrap_or_else(|_| json!([])))
        }
        WorkflowResource::Run { run_id } => {
            // 设计 §4.3 第 4 条：单 run 快照携带「已提交上界」`eventWatermark`；
            // 快照与水位必须一致读取（先取水位后取快照），否则并发提交会被跳过
            // （对抗审查 F4）。字段在既有投影之上追加，不改 `RunProgress` 的字段名
            //（TUI 投影依赖）。
            let Some((snapshot, watermark)) = runtime.run_snapshot_with_watermark(&run_id) else {
                return Err(ResourceReadError::NotFound(uri.to_string()));
            };
            let mut payload = serde_json::to_value(snapshot).unwrap_or(Value::Null);
            if let Some(object) = payload.as_object_mut() {
                object.insert("eventWatermark".to_string(), json!(watermark));
            }
            Ok(payload)
        }
        WorkflowResource::RunEvents { run_id } => {
            if runtime.run_snapshot(&run_id).is_none() {
                return Err(ResourceReadError::NotFound(uri.to_string()));
            }
            let page = parse_page_query(uri);
            let page = runtime
                .run_events()
                .page_where(page.after, page.limit, |event| event.run_id == run_id);
            Ok(event_page_payload(page))
        }
        WorkflowResource::RunTasks { run_id } => {
            // run-specific 资源一律以同一登记状态裁决（设计 §4.3 第 5 条：清理后
            // `workflow://runs/{runId}*` 返回 not-found）。**不**用任务表兜底：
            // run 只在 `RunStarted` 进度事件提交时进入 progress store
            //（`peri-workflow/src/progress.rs` 的 `commit`；其余事件只更新既有 run），
            // 而引擎在解析脚本后、执行脚本前就 emit `run_started`
            //（引擎 `@claude-code-best/workflow-engine`，构建时打进单文件 bundle），首个 `agent/task`
            // 必定在其后、且同一条 wire 通道按序处理，所以「任务先于 run 登记可见」
            // 不存在；任务表保留窗口（300s）长于 run 保留窗口，兜底只会让已清理
            // run 的资源复活。
            if runtime.run_snapshot(&run_id).is_none() {
                return Err(ResourceReadError::NotFound(uri.to_string()));
            }
            let page = parse_page_query(uri);
            let page = runtime
                .tasks()
                .events()
                .page_where(page.after, page.limit, |event| event.run_id == run_id);
            let baseline = runtime.tasks().baseline(Some(&run_id));
            Ok(task_page_payload(baseline, page))
        }
        WorkflowResource::Tasks => {
            let page = parse_page_query(uri);
            let page = runtime.tasks().events().page(page.after, page.limit);
            let baseline = runtime.tasks().baseline(None);
            Ok(task_page_payload(baseline, page))
        }
    }
}

/// 订阅唤醒对应的**规范** URI（分页参数不参与订阅身份）。
pub fn update_uri(update: &WorkflowResourceUpdate) -> String {
    match update {
        WorkflowResourceUpdate::RunsIndex => RUNS_URI.to_string(),
        WorkflowResourceUpdate::Run { run_id } => format!("{RUNS_PREFIX}{run_id}"),
        WorkflowResourceUpdate::RunEvents { run_id } => format!("{RUNS_PREFIX}{run_id}/events"),
        WorkflowResourceUpdate::RunTasks { run_id } => format!("{RUNS_PREFIX}{run_id}/tasks"),
        WorkflowResourceUpdate::Tasks => TASKS_URI.to_string(),
    }
}

fn event_page_payload<T: serde::Serialize>(page: EventLogPage<T>) -> Value {
    json!({
        "headSequence": page.head_sequence,
        "truncatedBefore": page.truncated_before,
        "entries": page.entries,
    })
}

/// 任务事件页 + 基线快照的合并正文（`workflow://tasks` 与 run 级任务视图）。
///
/// 读取顺序与不变量（对抗审查 F3）：
/// 1. **先读事件页、后读基线**：基线的 `head_sequence` ≥ 页上界。
/// 2. `activeTasks` 与 `headSequence` 是同一次临界区读取：基线水位内的状态变更
///    都在快照中可见（含「长期活跃、发布事件已被淘汰」的任务）。
/// 3. `entries` 覆盖 `(after, 页上界]`；读取方以 `entries` 内最大 sequence 推进
///    游标（分页可能未覆盖到 `headSequence`），并按 eventId 与基线去重（幂等）。
fn task_page_payload<T: serde::Serialize>(baseline: TaskBaseline, page: EventLogPage<T>) -> Value {
    json!({
        "headSequence": baseline.head_sequence,
        "truncatedBefore": baseline.truncated_before,
        "activeTasks": baseline.active_tasks,
        "entries": page.entries,
    })
}

#[cfg(test)]
#[path = "resources_test.rs"]
mod tests;
