//! WorkflowTool — LLM 可调用的 deferred tool，启动 workflow（fire-and-forget）。
//!
//! 工具立即返回 run_id，workflow 在后台执行。
//! 完成后通过 notification channel 注入 ReAct 循环。

use std::sync::Arc;

use async_trait::async_trait;
use peri_agent::tools::BaseTool;
use serde_json::Value;
use tokio::sync::{oneshot, watch};
use tracing::debug;
use tracing::warn;

use crate::journal::WorkflowJournalStore;
use crate::progress::WorkflowProgressStore;
use crate::registry::{WorkflowRunStatus, WorkflowTaskRegistry, WorkflowTaskResult};
use crate::runner::{WorkflowInput, WorkflowResult, WorkflowRunner};

/// Background task registry interface — avoids peri-workflow → peri-middlewares dependency.
///
/// Implemented by `peri_middlewares::BackgroundTaskRegistry`.
pub trait BgTaskRegistry: Send + Sync {
    fn register_workflow(&self, task_id: String, summary: String);
    fn complete_workflow(&self, task_id: &str, success: bool, output: String, duration_ms: u64);
}

/// Workflow 工具 — 启动 workflow（fire-and-forget）
pub struct WorkflowTool {
    runner: Arc<WorkflowRunner>,
    registry: Arc<WorkflowTaskRegistry>,
    progress_store: Arc<WorkflowProgressStore>,
    journal_store: Arc<WorkflowJournalStore>,
    /// Optional background task registry for unified task management
    bg_registry: Option<Arc<dyn BgTaskRegistry>>,
}

impl WorkflowTool {
    pub fn new(
        runner: Arc<WorkflowRunner>,
        registry: Arc<WorkflowTaskRegistry>,
        progress_store: Arc<WorkflowProgressStore>,
        journal_store: Arc<WorkflowJournalStore>,
    ) -> Self {
        Self {
            runner,
            registry,
            progress_store,
            journal_store,
            bg_registry: None,
        }
    }

    pub fn with_bg_registry(mut self, bg_registry: Arc<dyn BgTaskRegistry>) -> Self {
        self.bg_registry = Some(bg_registry);
        self
    }
}

#[async_trait]
impl BaseTool for WorkflowTool {
    fn name(&self) -> &str {
        "Workflow"
    }

    fn description(&self) -> &str {
        "Launch a workflow with multiple agents working in parallel or pipeline. \
         The workflow runs asynchronously — this tool returns immediately with a run_id. \
         When the workflow completes, you'll receive a notification with the result summary. \
         Use the workflow when you need to orchestrate multiple agents for complex tasks."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "script": {
                    "type": "string",
                    "description": "The workflow script (JavaScript ESM). \
                    Uses primitives: agent(), parallel(), pipeline(), phase(), log(), workflow(). \
                    Either `script` or `scriptPath` must be provided."
                },
                "args": {
                    "type": "object",
                    "description": "Optional arguments passed to the workflow script."
                },
                "maxConcurrency": {
                    "type": "number",
                    "description": "Maximum concurrent agents (default 3).",
                    "default": 3
                },
                "resumeFromRunId": {
                    "type": "string",
                    "description": "If provided, resume the workflow from the given run ID. \
                    The journal from the previous run will be loaded for cache-hit."
                },
                "name": {
                    "type": "string",
                    "description": "Optional workflow name (for display). \
                    If omitted, extracted from script's meta.name."
                },
                "scriptPath": {
                    "type": "string",
                    "description": "Path to a workflow script file (alternative to inline script). \
                    If provided, the file is read and used as the workflow script."
                }
            },
            "required": []
        })
    }

    fn timeout(&self) -> Option<std::time::Duration> {
        None
    }

    async fn invoke(
        &self,
        input: Value,
        _ctx: peri_agent::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        // scriptPath 优先于 inline script（GAP-09 命名 Workflow 支持）
        // 路径安全：限定在 cwd 内，拒绝越权读取
        let script_owned: String = if let Some(sp) = input["scriptPath"].as_str() {
            let cwd = std::path::PathBuf::from(self.runner.cwd());
            let cwd_canonical = cwd
                .canonicalize()
                .map_err(|e| format!("cwd not accessible: {}", e))?;
            let script_path = resolve_script_path(sp, &cwd_canonical)
                .map_err(|e| format!("Invalid scriptPath '{}': {}", sp, e))?;
            std::fs::read_to_string(&script_path)
                .map_err(|e| format!("Failed to read scriptPath '{}': {}", sp, e))?
        } else {
            input["script"]
                .as_str()
                .ok_or("missing 'script' or 'scriptPath' field")?
                .to_string()
        };
        let script = script_owned.as_str();

        let max_concurrency = input["maxConcurrency"].as_u64().unwrap_or(3) as u32;

        let args = input.get("args").cloned();

        // 解析 resumeFromRunId（GAP-04）— 必须通过安全校验
        let resume_from = if let Some(s) = input["resumeFromRunId"].as_str() {
            if !is_safe_run_id(s) {
                return Err(format!(
                    "Invalid resumeFromRunId '{}': must be a valid UUID without path traversal characters",
                    s
                )
                .into());
            }
            Some(s.to_string())
        } else {
            None
        };

        // name 参数优先于脚本 heuristic
        let workflow_name = input["name"]
            .as_str()
            .map(|s| s.to_string())
            .unwrap_or_else(|| extract_workflow_name(script));

        // 在 spawn 前生成 run_id，立即返回给 LLM（GAP-02）
        let run_id = uuid::Uuid::now_v7().to_string();

        let wf_input = WorkflowInput {
            script: script.to_string(),
            args,
            max_concurrency,
            budget_total: None,
            workflow_name: workflow_name.clone(),
            resume_from,
        };

        // Create channels for the runner
        // watch channel: 支持多接收者——fast_rx 用于快速失败检测，done_rx 用于通知任务
        let (done_tx, done_rx) = watch::channel::<Option<crate::runner::WorkflowResult>>(None);
        let (kill_tx, kill_rx) = oneshot::channel::<()>();

        // Spawn the workflow in background — 捕获 JoinHandle，不 move kill_tx 进去
        let runner = Arc::clone(&self.runner);
        let progress_store = Arc::clone(&self.progress_store);
        let journal_store = Arc::clone(&self.journal_store);
        let started_at = std::time::Instant::now();
        let run_id_clone = run_id.clone();

        let child_handle = tokio::spawn(async move {
            match runner
                .run(
                    run_id_clone,
                    wf_input,
                    progress_store,
                    journal_store,
                    done_tx,
                    kill_rx,
                )
                .await
            {
                Ok(()) => {
                    debug!("Workflow started successfully");
                }
                Err(e) => {
                    warn!(error = %e, "Workflow failed to start");
                }
            }
        });

        // 注册到 registry（并发限制检查）— GAP-03
        let script_preview: String = script.chars().take(100).collect();
        if let Err(e) = self.registry.register(crate::registry::WorkflowRun {
            run_id: run_id.clone(),
            workflow_name: workflow_name.clone(),
            script_preview,
            status: WorkflowRunStatus::Running,
            started_at,
            child_handle,
            kill_tx: Some(kill_tx),
        }) {
            return Err(format!("Workflow concurrency limit: {e}").into());
        }

        // 注册到统一后台任务注册表
        if let Some(ref bg) = self.bg_registry {
            bg.register_workflow(
                run_id.clone(),
                format!(
                    "{}: {}",
                    workflow_name,
                    script.chars().take(80).collect::<String>()
                ),
            );
        }

        // ─── 快速失败检测（1s 内 done 到来即同步报错）───
        let mut fast_rx = done_rx.clone(); // clone 用于快速失败检测
        let fast_result = tokio::select! {
            _ = fast_rx.changed() => {
                let val = fast_rx.borrow().clone();
                if val.is_none() {
                    // done_tx 被 drop 但从未 send → runner 在发送结果前就退出了
                    Some(WorkflowResult {
                        run_id: run_id.clone(),
                        status: "failed".to_string(),
                        return_value: None,
                        error: Some("workflow process exited before reporting result".to_string()),
                        stderr_tail: None,
                    })
                } else {
                    val
                }
            }
            _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => None,
        };

        if let Some(ref result) = fast_result {
            if result.status != "completed" {
                let error_msg = result
                    .error
                    .clone()
                    .unwrap_or_else(|| "workflow failed with no error details".to_string());
                let detail = result
                    .stderr_tail
                    .as_ref()
                    .map(|s| format!("\n\nstderr (last 20 lines):\n{}", s))
                    .unwrap_or_default();

                // 快速失败清理：update bg_registry so BgTaskArea transitions from ◎ to ✗
                let fast_duration = started_at.elapsed().as_millis() as u64;
                if let Some(ref bg) = self.bg_registry {
                    bg.complete_workflow(&run_id, false, String::new(), fast_duration);
                }
                // 同步标记 registry 为失败，发送通知给 agent
                self.registry.complete(
                    &run_id,
                    WorkflowTaskResult {
                        run_id: run_id.clone(),
                        workflow_name: workflow_name.clone(),
                        success: false,
                        status: WorkflowRunStatus::Failed,
                        duration_ms: fast_duration,
                        agent_count: 0,
                        tool_calls_count: 0,
                        error: Some(error_msg.clone()),
                        phase_summaries: Vec::new(),
                    },
                );

                return Err(format!(
                    "Workflow '{}' failed: {}{}",
                    workflow_name, error_msg, detail
                )
                .into());
            }
        }
        // ─── 快速失败检测结束 ───

        // Notification task: wait for completion → registry.complete()
        let registry_for_complete = Arc::clone(&self.registry);
        let notify_name = workflow_name.clone();
        let notify_started = started_at;
        let notify_run_id = run_id.clone();
        let notify_progress_store = Arc::clone(&self.progress_store);
        tokio::spawn(async move {
            let mut done_rx = done_rx;
            // 等待 watch channel 有值（workflow 完成）
            if done_rx.changed().await.is_err() {
                // sender dropped → workflow 异常
                warn!("Workflow done channel closed unexpectedly — marking as failed");
                let (agent_count, tool_calls_count) = notify_progress_store
                    .get_run_stats(&notify_run_id)
                    .unwrap_or((0, 0));
                let phase_summaries = notify_progress_store.get_phase_summaries(&notify_run_id);
                let duration = notify_started.elapsed().as_millis() as u64;
                registry_for_complete.complete(
                    &notify_run_id,
                    WorkflowTaskResult {
                        run_id: notify_run_id.clone(),
                        workflow_name: notify_name.clone(),
                        success: false,
                        status: WorkflowRunStatus::Failed,
                        duration_ms: duration,
                        agent_count,
                        tool_calls_count,
                        error: Some("workflow process exited unexpectedly".to_string()),
                        phase_summaries,
                    },
                );
                // bg.complete_workflow() 已移至 executor.rs 的 broadcast consumer 中
                // （在 Defer 入队之后调用），消除 active_count 提前归零的竞态窗口
                return;
            }
            let result = done_rx
                .borrow()
                .clone()
                .expect("watch value should be Some after changed() resolves");
            // 从 progress_store 获取真实 agent 数量与 tool count
            // 必须在 done_rx 之后读取——此时 workflow 已执行完毕，
            // progress_store 已被所有 progress/event RPC 填充。
            let (agent_count, tool_calls_count) = notify_progress_store
                .get_run_stats(&notify_run_id)
                .unwrap_or((0, 0));
            let phase_summaries = notify_progress_store.get_phase_summaries(&notify_run_id);
            let success = result.status == "completed";
            let status = match result.status.as_str() {
                "completed" => WorkflowRunStatus::Completed,
                "killed" => WorkflowRunStatus::Killed,
                _ => WorkflowRunStatus::Failed,
            };
            registry_for_complete.complete(
                &notify_run_id,
                WorkflowTaskResult {
                    run_id: notify_run_id.clone(),
                    workflow_name: notify_name,
                    success,
                    status,
                    duration_ms: notify_started.elapsed().as_millis() as u64,
                    agent_count,
                    tool_calls_count,
                    error: result.error,
                    phase_summaries,
                },
            );
            // bg.complete_workflow() 已移至 executor.rs 的 broadcast consumer 中
            // （在 Defer 入队之后调用），消除 active_count 提前归零的竞态窗口
        });

        Ok(format!(
            "Workflow '{}' started.\n\
             run_id: {}\n\
             \n\
             The workflow is running in the background.\n\
             You will be notified when it completes with a result summary.\n\
             Results will be saved to .claude/workflow-runs/{}/state.json",
            workflow_name, run_id, run_id
        ))
    }
}

/// 从脚本中提取 workflow 名称（简单 heuristic：查找 `name:` 后的第一个引号字符串）
fn extract_workflow_name(script: &str) -> String {
    // 尝试匹配 name: '...' 或 name: "..."
    if let Some(pos) = script.find("name:") {
        let after = &script[pos + 5..];
        let trimmed = after.trim_start();
        if trimmed.starts_with('\'') || trimmed.starts_with('"') {
            let quote = trimmed.chars().next().unwrap();
            let start = 1;
            if let Some(end) = trimmed[1..].find(quote) {
                return trimmed[start..start + end].to_string();
            }
        }
    }
    "unnamed".to_string()
}

/// 将用户提供的 scriptPath 解析为安全路径。
///
/// 1. 转为以 cwd 为基准的绝对路径
/// 2. 规范化（解析 `..` 和符号链接）
/// 3. 验证路径在 cwd 子树内，拒绝越权访问
fn resolve_script_path(
    raw: &str,
    cwd_canonical: &std::path::Path,
) -> Result<std::path::PathBuf, String> {
    let path = std::path::PathBuf::from(raw);
    let abs = if path.is_absolute() {
        path
    } else {
        cwd_canonical.join(&path)
    };
    let canonical = abs
        .canonicalize()
        .map_err(|e| format!("path not found: {e}"))?;
    if !canonical.starts_with(cwd_canonical) {
        return Err(format!("path '{}' is outside the working directory", raw));
    }
    Ok(canonical)
}

/// 验证 run_id 安全性：合法的 UUID 且不含路径遍历字符。
fn is_safe_run_id(s: &str) -> bool {
    // 禁止路径遍历字符
    if s.contains("..") || s.contains('/') || s.contains('\\') {
        return false;
    }
    // 必须为合法 UUID
    uuid::Uuid::parse_str(s).is_ok()
}
