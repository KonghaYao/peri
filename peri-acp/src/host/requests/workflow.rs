//! Workflow 命令 handler：list_runs / kill_agent / kill_run / resume（自
//! requests.rs 拆出，请求分发见 `host/requests.rs`）。
//!
//! 状态源迁移（设计 `docs/design/workflow-mcp.md` §6.3）：四 handler 不再持有
//! 会话级 middleware，改为经**会话作用域池**对池内 builtin `workflow` 实例做
//! `resources/read` 与 `tools/call`。`handle_request` 已按请求 sessionId 把 `cfg`
//! 解析为该会话的环境配置（`SessionEnvironment::cfg`，见 `requests.rs` 的
//! `environment` 分支），因此这里的 `cfg.mcp_pool` 就是**被请求会话**的池——
//! issue 2026-08-05 的「按 sessionId 定位、不取第一个可用会话」性质由装配结构
//! 保证，不再依赖 handler 内查找。
//!
//! 工具面契约（工具名 / 参数 / 文本口径）由 `mcp-packages/workflow` 单一定义，
//! 本文件只做协议映射与文本解读，解读集中在两个 `*_from_*` 函数：
//! - `WorkflowKill { runId, agentId? }` → 命中文本以 `killed.` 结尾；
//! - `Workflow { resumeFromRunId }` → 启动消息含 `run_id: <uuid>` 行
//!   （`peri-workflow` 的 `WorkflowStartOutcome::message`）。
//!
//! 工具面文本口径变更时，这两个函数与 `requests_workflow_cases_test` 同批复核。

use std::sync::Arc;

use serde_json::{json, Value};
use tracing::{info, warn};

use peri_acp_types::ports::McpPoolPort;

use super::super::AcpServerConfig;
use crate::transport::types::AcpError;

/// 池内 workflow builtin 实例名（与 `peri-middlewares` builtin 注册一致）。
const WORKFLOW_INSTANCE: &str = "workflow";

/// run 索引资源 URI（与 `peri_mcp_workflow::resources::RUNS_URI` 同值；
/// ACP 侧按协议面字面量引用，避免为非 emscripten 依赖加条件编译）。
const RUNS_URI: &str = "workflow://runs";

/// 会话无 workflow 消费面（无池）时的错误。
///
/// 迁移前该情形由「会话存在但无 middleware」分支报出同一文本（-32602
/// `session not found: {id}`）；兼容契约测试按文本断言（设计 §6.4：错误类型、
/// 排序与不存在 run 的行为保持现状语义）。
fn session_not_found(session_id: &str) -> AcpError {
    AcpError::new(-32602, format!("session not found: {session_id}"))
}

/// 解析被请求会话的池（`cfg` 已由 `handle_request` 解析到该会话的环境配置）。
fn session_pool<'a>(
    cfg: &'a AcpServerConfig,
    session_id: &str,
) -> Result<&'a Arc<dyn McpPoolPort>, AcpError> {
    cfg.mcp_pool
        .as_ref()
        .ok_or_else(|| session_not_found(session_id))
}

/// 池调用失败 → ACP 内部错误（-32603）。协议级与工具级失败在端口面同形
/// （`Err` + 服务端文本），原样透传文本，不伪造结果。
fn pool_error(error: String) -> AcpError {
    AcpError::new(-32603, error)
}

fn required_str<'a>(params: &'a Value, key: &str) -> Result<&'a str, AcpError> {
    params
        .get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| AcpError::new(-32602, format!("missing {key}")))
}

/// `WorkflowKill` 工具结果文本 → 是否命中（工具契约见文件头注释）。
fn killed_from_tool_text(text: &str) -> bool {
    text.trim_end().ends_with("killed.")
}

/// `Workflow` 启动消息 → 新 run id（工具契约见文件头注释）。
fn run_id_from_start_message(text: &str) -> Option<String> {
    text.lines()
        .find_map(|line| line.trim().strip_prefix("run_id: "))
        .map(str::trim)
        .filter(|run_id| !run_id.is_empty())
        .map(str::to_owned)
}

/// 全部 run 快照（`workflow://runs`）。
///
/// 兼容语义：会话没有 workflow 消费面时返回空列表（迁移前
/// `unwrap_or_default()` 的既有行为），不报错——未知 session 与无池同样落在
/// 这个分支；读到的资源正文必须是 JSON（run 索引），解析失败如实报错。
pub(super) async fn handle_list_runs(
    params: &Value,
    cfg: &AcpServerConfig,
) -> Result<Value, AcpError> {
    // 只做存在性校验（历史语义：未知 session 也返回空列表）。
    required_str(params, "sessionId")?;
    let Some(pool) = cfg.mcp_pool.as_ref() else {
        return Ok(json!({ "runs": [] }));
    };
    let text = pool
        .read_instance_resource(WORKFLOW_INSTANCE, RUNS_URI)
        .await
        .map_err(pool_error)?;
    let runs: Value = serde_json::from_str(&text).map_err(|error| {
        AcpError::new(
            -32603,
            format!("workflow runs resource is not valid JSON: {error}"),
        )
    })?;
    Ok(json!({ "runs": runs }))
}

/// 终止单个 workflow agent（`WorkflowKill { runId, agentId }`）。
pub(super) async fn handle_kill_agent(
    params: &Value,
    cfg: &AcpServerConfig,
) -> Result<Value, AcpError> {
    // 显式按请求 sessionId 定位（`cfg` 即该会话的环境配置）。
    let session_id = required_str(params, "sessionId")?;
    let run_id = required_str(params, "runId")?;
    let agent_id = params
        .get("agentId")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| AcpError::new(-32602, "missing agentId"))?;

    let pool = session_pool(cfg, session_id)?;
    let text = pool
        .call_instance_tool(
            WORKFLOW_INSTANCE,
            "WorkflowKill",
            json!({ "runId": run_id, "agentId": agent_id }),
            None,
        )
        .await
        .map_err(pool_error)?;
    let killed = killed_from_tool_text(&text);

    if killed {
        info!(run_id, agent_id, "Workflow agent killed via ACP");
    } else {
        warn!(run_id, agent_id, "Workflow agent kill failed (not found)");
    }
    Ok(json!({ "killed": killed }))
}

/// 终止整个 run（`WorkflowKill { runId }`）。
pub(super) async fn handle_kill_run(
    params: &Value,
    cfg: &AcpServerConfig,
) -> Result<Value, AcpError> {
    // 显式按请求 sessionId 定位（`cfg` 即该会话的环境配置）。
    let session_id = required_str(params, "sessionId")?;
    let run_id = required_str(params, "runId")?;

    let pool = session_pool(cfg, session_id)?;
    let text = pool
        .call_instance_tool(
            WORKFLOW_INSTANCE,
            "WorkflowKill",
            json!({ "runId": run_id }),
            None,
        )
        .await
        .map_err(pool_error)?;
    let killed = killed_from_tool_text(&text);

    if killed {
        info!(run_id, "Workflow run killed via ACP");
    } else {
        warn!(run_id, "Workflow run kill failed (not found)");
    }
    Ok(json!({ "killed": killed }))
}

/// 恢复 run（设计 §6.3：内核完成 state/journal 读取与启动，ACP 不拼装参数）。
///
/// ACP 只传 `resumeFromRunId`；新 run id 从内核启动消息中读取（工具契约）。
pub(super) async fn handle_resume(
    params: &Value,
    cfg: &AcpServerConfig,
) -> Result<Value, AcpError> {
    // 显式按请求 sessionId 定位（`cfg` 即该会话的环境配置）。
    let session_id = required_str(params, "sessionId")?;
    let run_id = required_str(params, "runId")?;

    let pool = session_pool(cfg, session_id)?;
    let text = pool
        .call_instance_tool(
            WORKFLOW_INSTANCE,
            "Workflow",
            json!({ "resumeFromRunId": run_id }),
            None,
        )
        .await
        .map_err(pool_error)?;
    let new_run_id = run_id_from_start_message(&text).ok_or_else(|| {
        AcpError::new(
            -32603,
            format!("workflow resume reply has no run id: {text}"),
        )
    })?;

    info!(old_run = %run_id, new_run = %new_run_id, "Workflow resumed via ACP");
    Ok(json!({
        "runId": new_run_id,
        "resumedFrom": run_id
    }))
}
