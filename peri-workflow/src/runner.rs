//! WorkflowRunner —— spawn node 子进程 + 消息循环 + agent 回调。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use peri_js_runtime::{JsExecutionHost, JsProcessSpec};
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::process::{Child, Command};
use tokio::sync::{oneshot, watch};
use tracing::{debug, info, warn};

use crate::error::WorkflowError;
use crate::journal::WorkflowJournalStore;
use crate::progress::WorkflowProgressStore;
use crate::protocol::*;
use crate::rpc::{IncomingMessage, RpcChannel};

/// 本地固定安装的 workflow engine 版本（与 npm 发布版本保持一致）。
/// npx 兜底必须带显式版本：`npx -y @peri-code/workflow` 在全局已有同名 bin
/// 时会静默复用旧版（CLI 子命令缺失、无任何输出），显式 `@<version>` 才能
/// 绕过该行为强制使用 registry 上的目标版本。
const WORKFLOW_NPM_VERSION: &str = "0.2.0";
const WORKFLOW_PACKAGE_NAME: &str = "@peri-code/workflow";
const WORKFLOW_ENTRY: &str = "dist/peri-workflow.js";
const INSTALL_TIMEOUT: Duration = Duration::from_secs(90);
const START_TIMEOUT: Duration = Duration::from_secs(15);
const WORKFLOW_PROTOCOL_VERSION: u32 = 1;
const WORKFLOW_BUILD_ID: &str = "@peri-code/workflow@0.2.0";
const NPX_FALLBACK_ENV: &str = "PERI_WORKFLOW_ALLOW_NPX_FALLBACK";
const EMBEDDED_WORKFLOW_ARTIFACT: &[u8] =
    include_bytes!("../../npm-packages/@peri-workflow/dist/peri-workflow.js");

/// 串行化本地安装（避免并发 workflow 同时触发安装）。
static INSTALL_LOCK: std::sync::LazyLock<tokio::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| tokio::sync::Mutex::new(()));

#[derive(Debug)]
struct WorkflowCommand {
    program: String,
    args: Vec<String>,
}

#[derive(serde::Deserialize)]
struct WorkflowPackageMetadata {
    name: String,
    version: String,
    main: String,
    #[serde(rename = "periProtocolVersion")]
    protocol_version: u32,
    #[serde(rename = "periBuildId")]
    build_id: String,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkflowStartAck {
    ok: bool,
    protocol_version: u32,
    build_id: String,
}

fn validate_start_ack(value: Value) -> Result<(), WorkflowError> {
    let ack: WorkflowStartAck = serde_json::from_value(value).map_err(|_| {
        WorkflowError::SpawnFailed("workflow/start returned an invalid handshake".into())
    })?;
    if !ack.ok
        || ack.protocol_version != WORKFLOW_PROTOCOL_VERSION
        || ack.build_id != WORKFLOW_BUILD_ID
    {
        return Err(WorkflowError::SpawnFailed(format!(
            "workflow artifact protocol mismatch: expected protocol {WORKFLOW_PROTOCOL_VERSION} build {WORKFLOW_BUILD_ID}"
        )));
    }
    Ok(())
}

fn workflow_prefix() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(
        PathBuf::from(home)
            .join(".peri")
            .join("workflow")
            .join(WORKFLOW_NPM_VERSION),
    )
}

/// 校验固定 artifact 的 package identity、版本和入口契约。
fn validate_workflow_artifact(base: &Path) -> Option<PathBuf> {
    let package_dir = base
        .join("node_modules")
        .join("@peri-code")
        .join("workflow");
    let metadata: WorkflowPackageMetadata =
        serde_json::from_slice(&std::fs::read(package_dir.join("package.json")).ok()?).ok()?;
    if metadata.name != WORKFLOW_PACKAGE_NAME
        || metadata.version != WORKFLOW_NPM_VERSION
        || metadata.main != WORKFLOW_ENTRY
        || metadata.protocol_version != WORKFLOW_PROTOCOL_VERSION
        || metadata.build_id != WORKFLOW_BUILD_ID
    {
        return None;
    }
    let entry = package_dir.join(&metadata.main);
    let canonical_package = package_dir.canonicalize().ok()?;
    let canonical_entry = entry.canonicalize().ok()?;
    if !canonical_entry.starts_with(&canonical_package)
        || !canonical_entry.metadata().ok()?.is_file()
        || canonical_entry.metadata().ok()?.len() == 0
    {
        return None;
    }
    Some(entry)
}

fn workflow_local_dist_in(base: &Path) -> Option<String> {
    validate_workflow_artifact(base).map(|path| path.to_string_lossy().into_owned())
}

fn workflow_local_dist() -> Option<String> {
    workflow_prefix().and_then(|prefix| workflow_local_dist_in(&prefix))
}

fn npx_fallback_allowed() -> bool {
    cfg!(test) || std::env::var_os(NPX_FALLBACK_ENV).as_deref() == Some(std::ffi::OsStr::new("1"))
}

/// 生产默认 fail closed；仅测试或显式 opt-in 时允许固定版本 npx fallback。
fn workflow_cmd() -> Result<WorkflowCommand, WorkflowError> {
    if let Some(dist) = workflow_local_dist() {
        return Ok(WorkflowCommand {
            program: "node".into(),
            args: vec![dist],
        });
    }
    if npx_fallback_allowed() {
        return Ok(WorkflowCommand {
            program: "npx".into(),
            args: vec![
                "-y".into(),
                format!("{WORKFLOW_PACKAGE_NAME}@{WORKFLOW_NPM_VERSION}"),
            ],
        });
    }
    Err(WorkflowError::SpawnFailed(format!(
        "validated workflow artifact {WORKFLOW_NPM_VERSION} is unavailable; set {NPX_FALLBACK_ENV}=1 to allow the network fallback"
    )))
}

async fn stop_install_child(child: &mut Child) {
    let _ = child.kill().await;
    let _ = child.wait().await;
}

async fn run_install_with_timeout(child: &mut Child, timeout: Duration) -> std::io::Result<bool> {
    match tokio::time::timeout(timeout, child.wait()).await {
        Ok(status) => status.map(|status| status.success()),
        Err(_) => {
            stop_install_child(child).await;
            Ok(false)
        }
    }
}

async fn publish_embedded_workflow_artifact() -> Result<(), WorkflowError> {
    if workflow_local_dist().is_some() {
        return Ok(());
    }
    let prefix = workflow_prefix().ok_or_else(|| {
        WorkflowError::SpawnFailed("HOME is unavailable for workflow artifact lookup".into())
    })?;
    let _guard = INSTALL_LOCK.lock().await;
    if workflow_local_dist().is_some() {
        return Ok(());
    }

    let parent = prefix.parent().ok_or_else(|| {
        WorkflowError::SpawnFailed("workflow artifact prefix has no parent".into())
    })?;
    tokio::fs::create_dir_all(parent).await?;
    if prefix.exists() {
        tokio::fs::remove_dir_all(&prefix).await?;
    }
    let staging = parent.join(format!(
        ".{WORKFLOW_NPM_VERSION}.staging-{}",
        uuid::Uuid::now_v7()
    ));
    let package = staging
        .join("node_modules")
        .join("@peri-code")
        .join("workflow");
    tokio::fs::create_dir_all(package.join("dist")).await?;
    tokio::fs::write(
        package.join("package.json"),
        serde_json::to_vec(&serde_json::json!({
            "name": WORKFLOW_PACKAGE_NAME,
            "version": WORKFLOW_NPM_VERSION,
            "main": WORKFLOW_ENTRY,
            "periProtocolVersion": WORKFLOW_PROTOCOL_VERSION,
            "periBuildId": WORKFLOW_BUILD_ID,
        }))?,
    )
    .await?;
    tokio::fs::write(package.join(WORKFLOW_ENTRY), EMBEDDED_WORKFLOW_ARTIFACT).await?;

    if validate_workflow_artifact(&staging).is_none() {
        let _ = tokio::fs::remove_dir_all(&staging).await;
        return Err(WorkflowError::SpawnFailed(
            "embedded workflow artifact failed validation".into(),
        ));
    }
    match tokio::fs::rename(&staging, &prefix).await {
        Ok(()) => {}
        Err(error) if validate_workflow_artifact(&prefix).is_some() => {
            let _ = tokio::fs::remove_dir_all(&staging).await;
            debug!(target: "workflow", error_kind = ?error.kind(), "another process published the workflow artifact");
        }
        Err(error) => {
            let _ = tokio::fs::remove_dir_all(&staging).await;
            return Err(WorkflowError::Io(error));
        }
    }
    Ok(())
}

/// 安装到同文件系统 staging，完整校验后通过 rename 发布。
async fn ensure_workflow_install() -> Result<(), WorkflowError> {
    if workflow_local_dist().is_some() {
        return Ok(());
    }
    let prefix = workflow_prefix().ok_or_else(|| {
        WorkflowError::SpawnFailed("HOME is unavailable for workflow artifact lookup".into())
    })?;
    let _guard = INSTALL_LOCK.lock().await;
    if workflow_local_dist().is_some() {
        return Ok(());
    }

    let parent = prefix.parent().ok_or_else(|| {
        WorkflowError::SpawnFailed("workflow artifact prefix has no parent".into())
    })?;
    tokio::fs::create_dir_all(parent).await?;
    let staging = parent.join(format!(
        ".{WORKFLOW_NPM_VERSION}.staging-{}",
        uuid::Uuid::now_v7()
    ));
    tokio::fs::create_dir(&staging).await?;

    let package = format!("{WORKFLOW_PACKAGE_NAME}@{WORKFLOW_NPM_VERSION}");
    let mut child = match Command::new("npm")
        .args(["install", "--prefix"])
        .arg(&staging)
        .arg(&package)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            let _ = tokio::fs::remove_dir_all(&staging).await;
            return Err(WorkflowError::Io(error));
        }
    };

    let installed = run_install_with_timeout(&mut child, INSTALL_TIMEOUT).await?;
    if !installed || validate_workflow_artifact(&staging).is_none() {
        stop_install_child(&mut child).await;
        let _ = tokio::fs::remove_dir_all(&staging).await;
        return Err(WorkflowError::SpawnFailed(
            "workflow artifact installation failed validation".into(),
        ));
    }

    match tokio::fs::rename(&staging, &prefix).await {
        Ok(()) => {}
        Err(error) if validate_workflow_artifact(&prefix).is_some() => {
            let _ = tokio::fs::remove_dir_all(&staging).await;
            debug!(target: "workflow", error_kind = ?error.kind(), "another installer published the workflow artifact");
        }
        Err(error) => {
            let _ = tokio::fs::remove_dir_all(&staging).await;
            return Err(WorkflowError::Io(error));
        }
    }
    if validate_workflow_artifact(&prefix).is_none() {
        return Err(WorkflowError::SpawnFailed(
            "published workflow artifact failed validation".into(),
        ));
    }
    info!(target: "workflow", version = WORKFLOW_NPM_VERSION, "installed validated workflow artifact");
    Ok(())
}

fn persist_failed_state(
    journal_store: &WorkflowJournalStore,
    run_id: &str,
    input: &WorkflowInput,
    started_at: &str,
    error: &WorkflowError,
) {
    let state = crate::journal::RunState {
        run_id: run_id.to_string(),
        workflow_name: input.workflow_name.clone(),
        status: "failed".to_string(),
        return_value: None,
        script: input.script.clone(),
        started_at: started_at.to_string(),
        finished_at: Some(chrono::Utc::now().to_rfc3339()),
        error: Some(format!("{error:#}")),
    };
    if let Err(write_error) = journal_store.write_state(run_id, &state) {
        warn!(target: "workflow", run_id, error = %write_error, "failed to persist workflow startup failure");
    }
}

// ─── Agent 回调 trait（3.0 批 2 波 1 迁入 peri-acp-types）────────────

pub use peri_acp_types::workflow::AgentExecutor;

trait RunScoped {
    fn run_id(&self) -> &str;
}

fn parse_run_scoped<T: DeserializeOwned + RunScoped>(
    params: Option<Value>,
    expected_run_id: &str,
) -> Result<T, &'static str> {
    let parsed: T = serde_json::from_value(params.unwrap_or(Value::Null))
        .map_err(|_| "invalid run-scoped RPC parameters")?;
    if parsed.run_id() != expected_run_id {
        return Err("runId does not match the active workflow run");
    }
    Ok(parsed)
}

impl RunScoped for AgentRunParams {
    fn run_id(&self) -> &str {
        &self.run_id
    }
}

impl RunScoped for WorkflowDoneParams {
    fn run_id(&self) -> &str {
        &self.run_id
    }
}

fn parse_agent_run_params(
    params: Option<Value>,
    expected_run_id: &str,
) -> Result<AgentRunParams, String> {
    parse_run_scoped(params, expected_run_id).map_err(str::to_string)
}

fn workflow_start_params(
    run_id: &str,
    input: &WorkflowInput,
    resume: Option<Vec<JournalEntry>>,
    cwd: &str,
) -> WorkflowStartParams {
    WorkflowStartParams {
        run_id: run_id.to_string(),
        script: input.script.clone(),
        args: input.args.clone(),
        budget_total: input.budget_total,
        max_concurrency: input.max_concurrency,
        resume,
        cwd: cwd.to_string(),
    }
}

// ─── 公开类型 ──────────────────────────────────────────────────

/// Workflow 输入参数
#[derive(Debug, Clone)]
pub struct WorkflowInput {
    pub script: String,
    pub args: Option<Value>,
    pub max_concurrency: u32,
    pub budget_total: Option<u64>,
    pub workflow_name: String,
    pub resume_from: Option<String>,
}

/// Workflow 执行结果
#[derive(Debug, Clone)]
pub struct WorkflowResult {
    pub run_id: String,
    pub status: String,
    pub return_value: Option<Value>,
    pub error: Option<String>,
    /// Node 进程 stderr 的最后 20 行（仅 status 为 "failed"/"killed" 时可能有值）。
    /// 用于诊断脚本加载/沙箱导致的快速失败。
    pub stderr_tail: Option<String>,
}

// ─── Journal RPC 参数反序列化 ───────────────────────────────────

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct JournalAppendParams {
    run_id: String,
    entry: crate::protocol::JournalEntry,
}

impl RunScoped for JournalAppendParams {
    fn run_id(&self) -> &str {
        &self.run_id
    }
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct JournalTruncateParams {
    run_id: String,
}

impl RunScoped for JournalTruncateParams {
    fn run_id(&self) -> &str {
        &self.run_id
    }
}

// ─── WorkflowRunner ────────────────────────────────────────────

pub struct WorkflowRunner {
    agent_executor: Arc<dyn AgentExecutor>,
    cwd: String,
    /// 活跃 workflow run 的 RPC 通道（run_id → channel），供 kill_agent 查找（GAP-07）。
    active_channels: dashmap::DashMap<String, Arc<RpcChannel>>,
    /// 进度事件接收通道（从 workflow agent 内部发送，合并到 msg_loop）
    progress_rx: parking_lot::Mutex<
        Option<tokio::sync::mpsc::UnboundedReceiver<crate::protocol::ProgressEvent>>,
    >,
}

impl WorkflowRunner {
    pub fn new(
        agent_executor: Arc<dyn AgentExecutor>,
        cwd: &str,
        progress_rx: Option<tokio::sync::mpsc::UnboundedReceiver<crate::protocol::ProgressEvent>>,
    ) -> Self {
        Self {
            agent_executor,
            cwd: cwd.to_string(),
            active_channels: dashmap::DashMap::new(),
            progress_rx: parking_lot::Mutex::new(progress_rx),
        }
    }

    /// 返回工作目录路径。
    pub fn cwd(&self) -> &str {
        &self.cwd
    }

    /// 杀死指定 workflow run 中的单个 agent（GAP-07）。
    ///
    /// 通过 `active_channels` 找到该 run 的 RpcChannel，
    /// 再通过 `pending_agents` 找到对应 agent 的 cancel 通道。
    /// 返回 `true` 表示成功杀死，`false` 表示 agent 不存在。
    pub async fn kill_agent(&self, run_id: &str, agent_id: u64) -> bool {
        if let Some(channel) = self.active_channels.get(run_id) {
            channel.kill_agent(run_id, agent_id).await
        } else {
            false
        }
    }

    /// 启动 workflow（后台执行，通过 channels 推送事件/通知）
    #[allow(clippy::too_many_arguments)]
    pub async fn run(
        &self,
        run_id: String,
        input: WorkflowInput,
        progress_store: Arc<WorkflowProgressStore>,
        journal_store: Arc<WorkflowJournalStore>,
        done_tx: watch::Sender<Option<WorkflowResult>>,
        kill_rx: oneshot::Receiver<()>,
    ) -> Result<(), WorkflowError> {
        // Helper: send failure result on done_tx before returning Err.
        // Ensures tool.rs's fast-failure detection always receives a result,
        // even when runner exits before spawning the msg_loop (e.g. binary not found).
        fn send_failure(
            tx: &watch::Sender<Option<WorkflowResult>>,
            journal_store: Option<&WorkflowJournalStore>,
            rid: &str,
            input: &WorkflowInput,
            started_at: &str,
            error: &WorkflowError,
        ) {
            if let Some(journal_store) = journal_store {
                persist_failed_state(journal_store, rid, input, started_at, error);
            }
            let _ = tx.send(Some(WorkflowResult {
                run_id: rid.to_string(),
                status: "failed".to_string(),
                return_value: None,
                error: Some(format!("{error:#}")),
                stderr_tail: None,
            }));
        }

        let started_at_iso = chrono::Utc::now().to_rfc3339();

        // 1. Persist script
        match journal_store.init_run(&run_id, &input.script) {
            Ok(()) => {}
            Err(e) => {
                let err = WorkflowError::Io(e);
                send_failure(
                    &done_tx,
                    Some(&journal_store),
                    &run_id,
                    &input,
                    &started_at_iso,
                    &err,
                );
                return Err(err);
            }
        }

        // 2. Resume: read old journal if resume_from is set
        let resume_entries = if let Some(ref old_run_id) = input.resume_from {
            journal_store.read_all(old_run_id).ok()
        } else {
            None
        };

        // 3. Publish the bundled artifact first so development, tests, and releases use
        // the same hermetic runtime. Network resolution remains an explicit fallback.
        if workflow_local_dist().is_none() {
            if let Err(error) = publish_embedded_workflow_artifact().await {
                if npx_fallback_allowed() {
                    warn!(target: "workflow", error_kind = %error, "embedded workflow artifact unavailable; trying explicit network fallback");
                    if let Err(install_error) = ensure_workflow_install().await {
                        warn!(target: "workflow", error_kind = %install_error, "workflow artifact install unavailable; using explicit npx fallback");
                    }
                } else {
                    send_failure(
                        &done_tx,
                        Some(&journal_store),
                        &run_id,
                        &input,
                        &started_at_iso,
                        &error,
                    );
                    return Err(error);
                }
            }
        }
        let command = match workflow_cmd() {
            Ok(command) => command,
            Err(e) => {
                send_failure(
                    &done_tx,
                    Some(&journal_store),
                    &run_id,
                    &input,
                    &started_at_iso,
                    &e,
                );
                return Err(e);
            }
        };
        let host = match JsExecutionHost::spawn(JsProcessSpec::new(command.program, command.args)) {
            Ok(host) => Arc::new(host),
            Err(error) => {
                let err = WorkflowError::from(error);
                send_failure(
                    &done_tx,
                    Some(&journal_store),
                    &run_id,
                    &input,
                    &started_at_iso,
                    &err,
                );
                return Err(err);
            }
        };

        // 4. Register channel for Workflow agent kill tracking (GAP-07)
        let channel = Arc::new(RpcChannel::new(host.channel()));
        self.active_channels
            .insert(run_id.clone(), Arc::clone(&channel));

        // 5. Generic host owns stdout/stderr readers; Adapter consumes routed messages.
        let mut msg_rx = host
            .take_incoming()
            .await
            .expect("new JavaScript host must expose its incoming receiver");

        // 7. Send workflow/start request
        let start_params = match serde_json::to_value(workflow_start_params(
            &run_id,
            &input,
            resume_entries,
            &self.cwd,
        )) {
            Ok(v) => v,
            Err(e) => {
                self.active_channels.remove(&run_id);
                let _ = host.kill().await;
                let err = WorkflowError::from(e);
                send_failure(
                    &done_tx,
                    Some(&journal_store),
                    &run_id,
                    &input,
                    &started_at_iso,
                    &err,
                );
                return Err(err);
            }
        };
        let start_resp = match tokio::time::timeout(
            START_TIMEOUT,
            channel.send_request("workflow/start", start_params),
        )
        .await
        {
            Ok(Ok(resp)) => resp,
            Ok(Err(_rpc_error)) => {
                self.active_channels.remove(&run_id);
                let _ = host.kill().await;
                let err = WorkflowError::SpawnFailed("workflow/start RPC failed".into());
                send_failure(
                    &done_tx,
                    Some(&journal_store),
                    &run_id,
                    &input,
                    &started_at_iso,
                    &err,
                );
                return Err(err);
            }
            Err(_timeout) => {
                self.active_channels.remove(&run_id);
                let _ = host.kill().await;
                let err = WorkflowError::SpawnFailed(
                    "workflow/start timed out (15s) — node process may have crashed".into(),
                );
                send_failure(
                    &done_tx,
                    Some(&journal_store),
                    &run_id,
                    &input,
                    &started_at_iso,
                    &err,
                );
                return Err(err);
            }
        };
        if let Err(err) = validate_start_ack(start_resp) {
            self.active_channels.remove(&run_id);
            let _ = host.kill().await;
            send_failure(
                &done_tx,
                Some(&journal_store),
                &run_id,
                &input,
                &started_at_iso,
                &err,
            );
            return Err(err);
        }

        // 8. Message loop (spawned task)
        let agent_executor = Arc::clone(&self.agent_executor);
        let channel_clone = Arc::clone(&channel);
        let journal_clone = Arc::clone(&journal_store);
        let progress_store_clone = Arc::clone(&progress_store);
        let run_id_clone = run_id.clone();
        let stderr_host = Arc::clone(&host);

        // Clone values needed by the kill branch before they're moved into msg_loop
        let kill_wf_name = input.workflow_name.clone();
        let kill_script = input.script.clone();
        let kill_started_at = started_at_iso.clone();

        // Clone done_tx for kill branch — must happen before async move consumes it
        let done_tx_for_kill = done_tx.clone();

        // 提取 progress_rx 供独立转发任务使用（Mutex::lock().take() 消费 Option 内的 receiver）
        let progress_rx_for_loop = self.progress_rx.lock().take();

        // 独立的 progress 转发任务：从 workflow agent 内部接收实时进度事件并写入 progress_store
        if let Some(mut progress_rx) = progress_rx_for_loop {
            let progress_store_for_progress = Arc::clone(&progress_store);
            let _progress_task = tokio::spawn(async move {
                while let Some(event) = progress_rx.recv().await {
                    progress_store_for_progress.apply_event(&event);
                }
            });
        }

        let mut msg_loop = tokio::spawn(async move {
            let mut final_result = WorkflowResult {
                run_id: run_id_clone.clone(),
                status: "failed".into(),
                return_value: None,
                error: None,
                stderr_tail: None,
            };

            let mut msg_count: usize = 0;
            let mut request_count: usize = 0;
            let mut response_count: usize = 0;
            let mut method_counts: HashMap<String, usize> = HashMap::new();

            while let Some(msg) = msg_rx.recv().await {
                msg_count += 1;
                match &msg {
                    IncomingMessage::Request { method, .. } => {
                        request_count += 1;
                        *method_counts.entry(method.clone()).or_default() += 1;
                    }
                    IncomingMessage::Response { .. } => {
                        response_count += 1;
                    }
                    IncomingMessage::ProtocolError(_) | IncomingMessage::ResourceLimit { .. } => {}
                }
                match msg {
                    IncomingMessage::Request { id, method, params } => match method.as_str() {
                        "agent/run" => {
                            // Parse params, spawn agent execution
                            let params = match parse_agent_run_params(params, &run_id_clone) {
                                Ok(params) => params,
                                Err(error) => {
                                    warn!(
                                        target: "workflow.rpc",
                                        error = %error,
                                        "agent/run rejected invalid params",
                                    );
                                    if let Some(id) = id {
                                        let _ = channel_clone
                                            .send_error(
                                                id,
                                                ERR_INVALID_PARAMS,
                                                "invalid agent/run params",
                                            )
                                            .await;
                                    }
                                    continue;
                                }
                            };
                            // Extract run_id + agent_id for kill tracking before moving params
                            let agent_run_id = params.run_id.clone();
                            let agent_id_num = params.agent_id;
                            // 注册提前到 spawn 之前（GAP-07 原子化）：kill_agent 与
                            // 注册之间不再有空窗（此前注册在 spawn 内，kill 先到会
                            // 漏杀且返回 false）；duplicate 拒绝在 spawn 前完成，
                            // 不产生孤儿 task。返回 (cancel_rx, 注册 token)。
                            let Some((cancel_rx, reg_token)) =
                                channel_clone.register_agent(&agent_run_id, agent_id_num, id)
                            else {
                                warn!(
                                    target: "workflow.rpc",
                                    run_id = %agent_run_id,
                                    agent_id = agent_id_num,
                                    "agent/run rejected duplicate active agentId",
                                );
                                if let Some(id) = id {
                                    let _ = channel_clone
                                        .send_error(
                                            id,
                                            ERR_INVALID_PARAMS,
                                            "duplicate active agentId",
                                        )
                                        .await;
                                }
                                continue;
                            };
                            let exec = Arc::clone(&agent_executor);
                            let ch = Arc::clone(&channel_clone);
                            let progress_for_agent = Arc::clone(&progress_store_clone);
                            tokio::spawn(async move {
                                // Execute with cancel support
                                let result = tokio::select! {
                                    r = exec.execute(params) => r,
                                    _ = cancel_rx => {
                                        crate::protocol::AgentRunResult::Dead {
                                            reason: Some("killed".into()),
                                            detail: Some("agent killed by user".into()),
                                        }
                                    }
                                };

                                // 完成归属：仅当注册仍由本 task 持有（未被 kill_agent
                                // 取走）时移除并返回 true；false 表示 kill 分支已发送
                                // error response，本 task 不得再发成功响应。
                                let owned =
                                    ch.deregister_agent(&agent_run_id, agent_id_num, reg_token);

                                // 从 progress store 补注 phase：engine 的 phase() 上下文仅通过
                                // progress 事件传递，不进入 AgentRunParams.phase（hooks.js:21 漏了）
                                // → agent_started 事件包含 phase，进度 store 先收到，此处补入结果。
                                let mut result = result;
                                if let AgentRunResult::Ok { ref mut phase, .. } = &mut result {
                                    if phase.is_none() {
                                        *phase = progress_for_agent
                                            .get_agent_phase(&agent_run_id, agent_id_num);
                                    }
                                }

                                // 响应门控：owned=false（注册已被 kill_agent 取走）或
                                // killed 结果（kill 分支已发送 error response）都跳过
                                // 响应，避免双重 JSON-RPC 响应违反协议规范。
                                // 其他 Dead 变体（no-structured-output / interrupted /
                                // runagent-threw）来自 executor 自身错误，仍需正常发送
                                // 响应，否则 Node Promise 永远 hang
                                if let Some(id) = id {
                                    let was_killed = matches!(
                                        result,
                                        AgentRunResult::Dead { reason: Some(ref r), .. } if r == "killed"
                                    );
                                    if owned && !was_killed {
                                        let result_val = serde_json::to_value(&result)
                                            .unwrap_or_else(|_| {
                                                serde_json::json!({
                                                    "kind": "dead",
                                                    "reason": "runagent-threw",
                                                    "detail": "serialize failed"
                                                })
                                            });
                                        let _ = ch.send_response(id, result_val).await;
                                    }
                                }
                            });
                        }
                        "progress/event" => {
                            if let Some(p) = params {
                                match serde_json::from_value::<ProgressEvent>(p.clone()) {
                                    Ok(event) if event.run_id() == run_id_clone => {
                                        debug!(
                                            target: "workflow.rpc",
                                            run_id = %run_id_clone,
                                            "progress/event: applied to store",
                                        );
                                        progress_store_clone.apply_event(&event);
                                    }
                                    Ok(_) => {
                                        warn!(target: "workflow", "progress/event rejected for inactive run");
                                    }
                                    Err(_) => {
                                        warn!(target: "workflow", "progress/event: invalid parameters")
                                    }
                                }
                            }
                        }
                        "journal/append" => {
                            if let Some(p) = params {
                                if let Ok(parsed) =
                                    parse_run_scoped::<JournalAppendParams>(Some(p), &run_id_clone)
                                {
                                    if let Err(e) =
                                        journal_clone.append(&parsed.run_id, &parsed.entry)
                                    {
                                        warn!(target: "workflow", run_id = %parsed.run_id, error = %e, "journal/append: write failed");
                                    }
                                }
                            }
                        }
                        "journal/truncate" => {
                            if let Some(p) = params {
                                if let Ok(parsed) = parse_run_scoped::<JournalTruncateParams>(
                                    Some(p),
                                    &run_id_clone,
                                ) {
                                    if let Err(e) = journal_clone.truncate(&parsed.run_id) {
                                        warn!(target: "workflow", run_id = %parsed.run_id, error = %e, "journal/truncate: write failed");
                                    }
                                }
                            }
                        }
                        "log" => {
                            // Node log bodies may contain user script data or credentials.
                            debug!(target: "workflow:node", "workflow node log received");
                        }
                        "workflow/done" => {
                            if let Ok(done) =
                                parse_run_scoped::<WorkflowDoneParams>(params, &run_id_clone)
                            {
                                if done.status != "completed" {
                                    warn!(
                                        target: "workflow",
                                        run_id = %done.run_id,
                                        status = %done.status,
                                        "workflow ended non-completed"
                                    );
                                }
                                let processed_return_value = done.return_value.map(|mut v| {
                                    if v.is_object() {
                                        let journal_for_extract = Arc::clone(&journal_clone);
                                        let _extracted = crate::journal::extract_long_texts(
                                            &mut v,
                                            &done.run_id,
                                            &journal_for_extract,
                                            200,
                                        );
                                    }
                                    v
                                });
                                final_result = WorkflowResult {
                                    run_id: done.run_id.clone(),
                                    status: done.status.clone(),
                                    return_value: processed_return_value,
                                    error: done.error.clone(),
                                    stderr_tail: None,
                                };
                                break;
                            }
                        }
                        _ => {
                            warn!(target: "workflow", "unknown method from node: {method}");
                            if let Some(id) = id {
                                let _ = channel_clone
                                    .send_error(id, ERR_METHOD_NOT_FOUND, "Method not found")
                                    .await;
                            }
                        }
                    },
                    IncomingMessage::Response { .. } => {
                        debug!(target: "workflow", "orphan response received");
                    }
                    IncomingMessage::ProtocolError(error) => {
                        final_result.error = Some(error);
                        break;
                    }
                    IncomingMessage::ResourceLimit { .. } => {
                        final_result.error = Some("JavaScript RPC resource limit exceeded".into());
                        break;
                    }
                }
            }

            tracing::info!(
                target: "workflow",
                run_id = %run_id_clone,
                total_msgs = msg_count,
                requests = request_count,
                responses = response_count,
                method_count = method_counts.len(),
                final_status = %final_result.status,
                "msg_loop exiting — summary"
            );

            // Write state.json
            let stderr_tail = stderr_host.stderr_tail();
            let state = crate::journal::RunState {
                run_id: final_result.run_id.clone(),
                workflow_name: input.workflow_name.clone(),
                status: final_result.status.clone(),
                return_value: final_result.return_value.clone(),
                script: input.script.clone(),
                started_at: started_at_iso,
                finished_at: Some(chrono::Utc::now().to_rfc3339()),
                error: final_result.error.clone(),
            };
            tracing::debug!(
                target: "workflow",
                run_id = %final_result.run_id,
                "calling write_state"
            );
            if let Err(e) = journal_clone.write_state(&final_result.run_id, &state) {
                warn!(target: "workflow", run_id = %final_result.run_id, error = %e, "write_state failed");
            } else {
                tracing::info!(
                    target: "workflow",
                    run_id = %final_result.run_id,
                    "write_state succeeded"
                );
            }

            // 收尾收敛 progress_store：msg_loop 自然退出（Node 崩溃/stdout 关闭）时
            // Node 侧 run_done 事件不会到达，run 会永久停留在 Running（幽灵 running，
            // 与 kill 分支同源，issue 2026-08-05）。status 取 final_result.status：
            // completed 路径与 Node 已发的 RunDone 幂等，failed 路径修复永久 Running。
            // 与 kill 分支时序无冲突：kill 会 abort 本 msg_loop，收尾不执行。
            progress_store_clone.apply_event(&ProgressEvent::RunDone {
                run_id: final_result.run_id.clone(),
                status: final_result.status.clone(),
                return_value: None,
                error: final_result.error.clone(),
            });

            final_result.stderr_tail = stderr_tail;
            let _ = done_tx.send(Some(final_result));
        });

        // 9. Wait for kill signal or message loop completion
        let journal_clone2 = Arc::clone(&journal_store);
        tokio::select! {
            biased;
            _ = kill_rx => {
                // 超时保护：Node crash 时不会阻塞 (M-ARCH6)
                let _ = tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    channel.send_request("workflow/kill", serde_json::json!({"runId": run_id})),
                )
                .await;
                let _ = host.kill().await;

                // Abort msg_loop 防止 state.json 和 done_tx 被覆写为 "failed"
                // （msg_loop 检测到 stdout 关闭后会以默认 status="failed" 写 state.json + done_tx，
                //  而 watch channel 后到值会覆盖先到值使 kill 事实丢失）
                msg_loop.abort();
                let _ = (&mut msg_loop).await;

                // 写入 killed state.json
                let stderr_tail = host.stderr_tail();
                let state = crate::journal::RunState {
                    run_id: run_id.clone(),
                    workflow_name: kill_wf_name,
                    status: "killed".to_string(),
                    return_value: None,
                    script: kill_script,
                    started_at: kill_started_at,
                    finished_at: Some(chrono::Utc::now().to_rfc3339()),
                    error: Some("workflow killed by user".to_string()),
                };
                let _ = journal_clone2.write_state(&run_id, &state);

                // 复用 reducer 标记 Killed 终态（与 Node 正常路径 run_done 同一状态机入口）：
                // msg_loop 已被 abort，Node 侧 run_done 不会到达；不标记则 progress_store 会
                // 永久保留 Running 条目（workflow/list_runs 幽灵 running）
                progress_store.apply_event(&ProgressEvent::RunDone {
                    run_id: run_id.clone(),
                    status: "killed".to_string(),
                    return_value: None,
                    error: Some("workflow killed by user".to_string()),
                });

                // 发送 done_tx（kill 分支作为唯一出口，确保通知任务收到 "killed" 状态）
                let killed_result = WorkflowResult {
                    run_id: run_id.clone(),
                    status: "killed".to_string(),
                    return_value: None,
                    error: Some("workflow killed by user".to_string()),
                    stderr_tail,
                };
                let _ = done_tx_for_kill.send(Some(killed_result));
            }
            _ = &mut msg_loop => {
                // Message loop completed naturally
            }
        }

        // Cleanup: ensure child process is terminated（防止僵尸进程）
        let _ = host.kill().await;

        // Cleanup: remove channel from active tracking (GAP-07)
        self.active_channels.remove(&run_id);

        // Cleanup old runs from journal
        let _ = journal_store.cleanup_old_runs();

        // Cleanup completed runs from progress store（防止内存泄漏 S-PERF4）
        progress_store.cleanup_completed();

        Ok(())
    }
}

#[cfg(test)]
#[path = "runner_test.rs"]
mod tests;
