//! WorkflowTool — LLM 可调用的 deferred tool，启动 workflow（fire-and-forget）。
//!
//! 工具立即返回 run_id，workflow 在后台执行。
//! 完成后通过 notification channel 注入 ReAct 循环。

use std::sync::Arc;

use async_trait::async_trait;
use peri_acp_types::event::BackgroundTaskResult;
use peri_acp_types::tasks::{BgTaskKind, BgTaskRegistration, TaskManager};
use peri_acp_types::tools::BaseTool;
use serde_json::Value;
use tokio::sync::{oneshot, watch};
use tracing::debug;
use tracing::warn;

use crate::journal::WorkflowJournalStore;
use crate::progress::WorkflowProgressStore;
use crate::registry::{WorkflowRunStatus, WorkflowTaskRegistry, WorkflowTaskResult};
use crate::runner::{receive_workflow_result, WorkflowInput, WorkflowResult, WorkflowRunner};

const MAX_SAFE_BUDGET_TOTAL: u64 = 9_007_199_254_740_991;
const MAX_SAFE_INTEGER: u64 = MAX_SAFE_BUDGET_TOTAL;
const MAX_CONCURRENCY_CAP: u64 = 16;
const PREFLIGHT_ARTIFACT: &[u8] = crate::runner::WORKFLOW_ARTIFACT_BYTES;

/// Workflow 工具 — 启动 workflow（fire-and-forget）
pub struct WorkflowTool {
    runner: Arc<WorkflowRunner>,
    registry: Arc<WorkflowTaskRegistry>,
    progress_store: Arc<WorkflowProgressStore>,
    journal_store: Arc<WorkflowJournalStore>,
    /// 统一后台任务管理（经 acp-types 契约接口；Agent 层 per-session
    /// TaskManager 实现，装配注入，取消转发到 [`WorkflowTaskRegistry::kill`]）
    bg_registry: Option<Arc<dyn TaskManager>>,
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

    pub fn with_bg_registry(mut self, bg_registry: Arc<dyn TaskManager>) -> Self {
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
                "budgetTotal": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": MAX_SAFE_INTEGER,
                    "description": "Maximum total token budget for this workflow. Omit for no explicit budget."
                },
                "maxAgents": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": MAX_SAFE_INTEGER,
                    "description": "Fail-safe host limit for live agent attempts. Resume cache hits are not charged."
                },
                "maxToolCalls": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": MAX_SAFE_INTEGER,
                    "description": "Fail-safe host limit for tool calls reported by completed live agents."
                },
                "maxElapsedMs": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": MAX_SAFE_INTEGER,
                    "description": "Fail-safe host wall-clock limit in milliseconds."
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
                "strictPreflight": {
                    "type": "boolean",
                    "default": false,
                    "description": "Reject unless workflow primitives and graph can be statically validated. The current engine cannot provide that proof."
                },
                "writeIntent": {
                    "description": "Declarative repository write ownership. Omit only for legacy runs; omitted intent can never produce a deliverable result.",
                    "oneOf": [
                        {"type": "object", "properties": {"kind": {"const": "read_only"}}, "required": ["kind"], "additionalProperties": false},
                        {
                            "type": "object",
                            "properties": {
                                "kind": {"const": "write"},
                                "repo_root": {"type": "string"},
                                "cwd": {"type": "string"},
                                "path_allowlist": {"type": "array", "items": {"type": "string"}},
                                "head_may_change": {"type": "boolean", "default": false},
                                "commit_required": {"type": "boolean"}
                            },
                            "required": ["kind", "repo_root", "cwd", "path_allowlist"],
                            "additionalProperties": false
                        }
                    ]
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
        _ctx: peri_acp_types::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        if input["strictPreflight"].as_bool().unwrap_or(false) {
            return Err("strict preflight is unavailable: workflow primitives and graph cannot be statically validated".into());
        }

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

        let max_concurrency =
            parse_bounded_integer(&input, "maxConcurrency", Some(3), MAX_CONCURRENCY_CAP)?
                .expect("maxConcurrency has a default") as u32;
        let budget_total = parse_budget_total(&input)?;
        let limits = crate::protocol::WorkflowLimits {
            max_agents: parse_bounded_integer(&input, "maxAgents", None, MAX_SAFE_INTEGER)?,
            max_tool_calls: parse_bounded_integer(&input, "maxToolCalls", None, MAX_SAFE_INTEGER)?,
            max_elapsed_ms: parse_bounded_integer(&input, "maxElapsedMs", None, MAX_SAFE_INTEGER)?,
        };

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

        let write_intent = input
            .get("writeIntent")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|error| format!("Invalid writeIntent: {error}"))?;

        preflight_validate_script(script).await?;
        let git_baseline = crate::journal::GitBaseline::capture_for_intent(
            std::path::Path::new(self.runner.cwd()),
            write_intent.as_ref(),
        )
        .map_err(|error| format!("Workflow preflight failed: {error}"))?;

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
            budget_total,
            limits,
            workflow_name: workflow_name.clone(),
            resume_from,
            write_intent,
            git_baseline,
        };

        // Create channels for the runner
        // watch channel: 支持多接收者——fast_rx 用于快速失败检测，done_rx 用于通知任务
        let (done_tx, done_rx) = watch::channel::<Option<crate::runner::WorkflowResult>>(None);
        let (kill_tx, kill_rx) = oneshot::channel::<()>();
        let started_at = std::time::Instant::now();

        // 先原子占用 registry 并发槽，再 spawn，避免并发失败产生孤儿 run。
        let script_preview: String = script.chars().take(100).collect();
        self.registry
            .reserve(crate::registry::WorkflowRun {
                run_id: run_id.clone(),
                workflow_name: workflow_name.clone(),
                script_preview,
                status: WorkflowRunStatus::Running,
                started_at,
                child_handle: None,
                kill_tx: Some(kill_tx),
            })
            .map_err(|error| format!("Workflow concurrency limit: {error}"))?;

        let runner = Arc::clone(&self.runner);
        let progress_store = Arc::clone(&self.progress_store);
        let journal_store = Arc::clone(&self.journal_store);
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
        self.registry.attach_child(&run_id, child_handle);

        // 注册到统一后台任务注册表（经 acp-types 契约，装配注入的 Agent 层 TaskManager）
        if let Some(ref bg) = self.bg_registry {
            // 携带 kill 闭包：session/cancel-bg-task 时转发到 WorkflowTaskRegistry::kill
            // （kill_tx 的唯一持有者，与 workflow/kill_run RPC 同一通道）。
            let kill_registry = Arc::clone(&self.registry);
            let kill_run_id = run_id.clone();
            if let Err(e) = bg.register(BgTaskRegistration {
                task_id: run_id.clone(),
                kind: BgTaskKind::Workflow,
                summary: format!(
                    "{}: {}",
                    workflow_name,
                    script.chars().take(80).collect::<String>()
                ),
                pid: None,
                kill: Some(Box::new(move || {
                    let _ = kill_registry.kill(&kill_run_id);
                })),
            }) {
                warn!(error = %e, "workflow bg registry: register failed");
            }
        }

        // ─── 快速失败检测（1s 内 done 到来即同步报错）───
        let mut fast_rx = done_rx.clone(); // clone 用于快速失败检测
        let fast_result = tokio::select! {
            result = receive_workflow_result(&mut fast_rx) => {
                Some(result.unwrap_or_else(|| WorkflowResult {
                    run_id: run_id.clone(),
                    status: "failed".to_string(),
                    return_value: None,
                    error: Some("workflow process exited before reporting result".to_string()),
                    post_processing_status:
                        peri_acp_types::workflow::PostProcessingStatus::Failed,
                    delivery_status: peri_acp_types::workflow::DeliveryStatus::Blocked,
                    stderr_tail: None,
                }))
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

                // 快速失败清理：complete 使 BgTaskArea 从 ◎ 过渡到 ✗
                let fast_duration = started_at.elapsed().as_millis() as u64;
                if let Some(ref bg) = self.bg_registry {
                    let result = BackgroundTaskResult {
                        task_id: run_id.clone(),
                        agent_name: "workflow".to_string(),
                        prompt_summary: String::new(),
                        success: false,
                        output: String::new(),
                        tool_calls_count: 0,
                        duration_ms: fast_duration,
                        child_thread_id: None,
                        timed_out: false,
                    };
                    bg.complete(&run_id, result);
                }
                // 同步标记 registry 为失败，发送通知给 agent
                self.registry.complete(
                    &run_id,
                    WorkflowTaskResult {
                        run_id: run_id.clone(),
                        workflow_name: workflow_name.clone(),
                        success: false,
                        status: WorkflowRunStatus::Failed,
                        execution_status: peri_acp_types::workflow::ExecutionStatus::Failed,
                        acceptance_status: peri_acp_types::workflow::AcceptanceStatus::Unknown,
                        post_processing_status:
                            peri_acp_types::workflow::PostProcessingStatus::Blocked,
                        delivery_status: peri_acp_types::workflow::DeliveryStatus::Blocked,
                        state_artifact_exists: self
                            .journal_store
                            .run_dir(&run_id)
                            .join("state.json")
                            .is_file(),
                        duration_ms: fast_duration,
                        agent_count: 0,
                        tool_calls_count: 0,
                        error: Some(error_msg.clone()),
                        phase_summaries: Vec::new(),
                        attempts: self
                            .journal_store
                            .read_attempts(&run_id)
                            .unwrap_or_default(),
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
        let notify_journal_store = Arc::clone(&self.journal_store);
        tokio::spawn(async move {
            let mut done_rx = done_rx;
            let Some(result) = receive_workflow_result(&mut done_rx).await else {
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
                        execution_status: peri_acp_types::workflow::ExecutionStatus::Failed,
                        acceptance_status: peri_acp_types::workflow::AcceptanceStatus::Unknown,
                        post_processing_status:
                            peri_acp_types::workflow::PostProcessingStatus::Blocked,
                        delivery_status: peri_acp_types::workflow::DeliveryStatus::Blocked,
                        state_artifact_exists: notify_journal_store
                            .run_dir(&notify_run_id)
                            .join("state.json")
                            .is_file(),
                        duration_ms: duration,
                        agent_count,
                        tool_calls_count,
                        error: Some("workflow process exited unexpectedly".to_string()),
                        phase_summaries,
                        attempts: notify_journal_store
                            .read_attempts(&notify_run_id)
                            .unwrap_or_default(),
                    },
                );
                // bg.complete_workflow() 已移至 executor.rs 的 broadcast consumer 中
                // （在 Defer 入队之后调用），消除 active_count 提前归零的竞态窗口
                return;
            };
            // 从 progress_store 获取真实 agent 数量与 tool count
            // 必须在 done_rx 之后读取——此时 workflow 已执行完毕，
            // progress_store 已被所有 progress/event RPC 填充。
            let (agent_count, tool_calls_count) = notify_progress_store
                .get_run_stats(&notify_run_id)
                .unwrap_or((0, 0));
            let phase_summaries = notify_progress_store.get_phase_summaries(&notify_run_id);
            let acceptance_status = notify_journal_store
                .read_state(&notify_run_id)
                .map(|state| state.acceptance_status)
                .unwrap_or_default();
            let success = result.status == "completed";
            let status = match result.status.as_str() {
                "completed" => WorkflowRunStatus::Completed,
                "killed" => WorkflowRunStatus::Killed,
                _ => WorkflowRunStatus::Failed,
            };
            let execution_status = match result.status.as_str() {
                "completed" => peri_acp_types::workflow::ExecutionStatus::Completed,
                "killed" => peri_acp_types::workflow::ExecutionStatus::Killed,
                _ => peri_acp_types::workflow::ExecutionStatus::Failed,
            };
            registry_for_complete.complete(
                &notify_run_id,
                WorkflowTaskResult {
                    run_id: notify_run_id.clone(),
                    workflow_name: notify_name,
                    success,
                    status,
                    execution_status,
                    acceptance_status,
                    post_processing_status: result.post_processing_status,
                    delivery_status: result.delivery_status,
                    state_artifact_exists: notify_journal_store
                        .run_dir(&notify_run_id)
                        .join("state.json")
                        .is_file(),
                    duration_ms: notify_started.elapsed().as_millis() as u64,
                    agent_count,
                    tool_calls_count,
                    error: result.error,
                    phase_summaries,
                    attempts: notify_journal_store
                        .read_attempts(&notify_run_id)
                        .unwrap_or_default(),
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

fn parse_budget_total(input: &Value) -> Result<Option<u64>, String> {
    parse_bounded_integer(input, "budgetTotal", None, MAX_SAFE_INTEGER)
}

fn parse_bounded_integer(
    input: &Value,
    field: &str,
    default: Option<u64>,
    maximum: u64,
) -> Result<Option<u64>, String> {
    let Some(value) = input.get(field) else {
        return Ok(default);
    };
    let Some(value) = value.as_u64() else {
        return Err(format!(
            "'{field}' must be an integer between 1 and {maximum}"
        ));
    };
    if !(1..=maximum).contains(&value) {
        return Err(format!(
            "'{field}' must be an integer between 1 and {maximum}"
        ));
    }
    Ok(Some(value))
}

async fn preflight_validate_script(script: &str) -> Result<(), String> {
    let temp =
        std::env::temp_dir().join(format!("peri-workflow-preflight-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir(&temp)
        .map_err(|error| format!("Workflow preflight unavailable: {error}"))?;
    let artifact = temp.join("peri-workflow.js");
    let source = temp.join("script.js");
    let result = (|| {
        std::fs::write(&artifact, PREFLIGHT_ARTIFACT)?;
        std::fs::write(&source, script)?;
        Ok::<_, std::io::Error>(())
    })();
    if let Err(error) = result {
        let _ = std::fs::remove_dir_all(&temp);
        return Err(format!("Workflow preflight unavailable: {error}"));
    }

    let output = tokio::process::Command::new("node")
        .arg(&artifact)
        .arg("validate")
        .arg(&source)
        .arg("--json")
        .output()
        .await
        .map_err(|error| format!("Workflow preflight unavailable: {error}"));
    let _ = std::fs::remove_dir_all(&temp);
    let output = output?;
    if output.status.success() {
        return Ok(());
    }
    let message = serde_json::from_slice::<Value>(&output.stdout)
        .ok()
        .and_then(|value| value["errors"].as_array().cloned())
        .and_then(|errors| errors.first().and_then(Value::as_str).map(str::to_string))
        .unwrap_or_else(|| "script validation failed".to_string());
    Err(format!("Workflow preflight failed: {message}"))
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

#[cfg(test)]
#[path = "tool_test.rs"]
mod tests;
