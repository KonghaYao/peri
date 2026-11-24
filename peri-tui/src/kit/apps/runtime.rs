//! Apps 运行时：发现轮询、attach → resources/read → view session → SSE。
//!
//! ## 边界
//!
//! - 只在 `PERI_MCP_APPS_TUI` 真值 + `PERI_BROWSER_VIEW_URL` + token 全部就绪时
//!   启动；否则 [`spawn_if_enabled`] 返回 `None`，**零 ACP 请求、零轮询、零 HTTP**。
//! - `apps/attach` + `resources/read` 不触发任何业务工具调用（A1 判据）；本模块
//!   只发送 `peri/mcp/apps/list|attach|request|detach` 四个方法，测试锁定。
//! - 多视图（A6）：每个 view 独立 attach / seq / SSE task / 取消令牌；回执顺序
//!   不影响归属。
//! - 画布点击（A2）：面板只上报真实 `MouseEvent` 经 mapping 得到的 CSS 点；
//!   本模块按 view 自己的网关会话转发 `/click`，坐标不落日志。
//! - App 回调（A3/A5）：`app-request` → 每视图有界队列 → `apps/request`
//!   （`tools/call`，同 `{attachment, epoch, seq}` 重试）→ `/app-results` 回填
//!   原始 outcome；不产生 LLM turn，也不直接调用业务工具。
//! - 关闭：本地关闭、面板关闭、TUI 退出都会对网关 `DELETE /view/sessions/{id}`
//!   （attach 的 detach 为尽力而为的顺带清理）。
//! - 日志只出现错误码/数量，不出现 token、HTML、帧 base64 或 URL query。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use peri_acp_types::mcp_apps_facade::{
    AppsAttachRequest, AppsAttachResponse, AppsDetachRequest, AppsListRequest, AppsListResponse,
    AppsRequestRequest, AppsRequestResponse,
};

use crate::acp_client::AcpTuiClient;

use super::config::{self, AppsAvailability, AppsConfig};
use super::mapping::ViewportPoint;
use super::state::{self, AppEntry, AppView, AppsRuntimeState};
use super::view_client::{AppsViewClient, ViewSessionApi};

mod relay;
mod view_task;

use self::view_task::{ViewContext, run_view};

/// 单次 ACP RPC 的外层超时（`send_raw_request` 自身无超时）。
const ACP_TIMEOUT: Duration = Duration::from_secs(15);
/// 关闭/清理的单步预算：detach、DELETE 都不无限等待。
pub(super) const CLEANUP_TIMEOUT: Duration = Duration::from_secs(3);
/// `apps/request`（含 tools/call 中继）的 ACP 外层超时：必须覆盖 facade 的
/// joinedPending ≤30s 等待，否则会把「仍在执行」误判为传输超时。
pub(super) const RELAY_RPC_TIMEOUT: Duration = Duration::from_secs(40);

// ── ACP RPC 抽象 ────────────────────────────────────────────────────────────

/// ACP 侧错误：只保留错误码（`data.kind` 优先，退回 JSON-RPC code）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RpcError {
    code: String,
}

impl RpcError {
    #[must_use]
    pub fn new(code: impl Into<String>) -> Self {
        let raw = code.into();
        let code = raw
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
            .then_some(raw)
            .filter(|code| !code.is_empty() && code.len() <= 64)
            .unwrap_or_else(|| "apps_rpc_error".to_string());
        Self { code }
    }

    #[must_use]
    pub fn code(&self) -> &str {
        &self.code
    }

    fn from_acp(error: &peri_acp::transport::types::AcpError) -> Self {
        let kind = error
            .data
            .as_ref()
            .and_then(|data| data.get("kind"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| format!("acp_{}", error.code));
        Self::new(kind)
    }
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.code)
    }
}

impl std::error::Error for RpcError {}

/// 运行时依赖的 `peri/mcp/apps/*` 面（测试注入 fake）。
#[async_trait]
pub trait AppsRpc: Send + Sync {
    fn current_session_id(&self) -> Option<String>;
    async fn list(&self, session_id: &str) -> Result<AppsListResponse, RpcError>;
    async fn attach(&self, request: AppsAttachRequest) -> Result<AppsAttachResponse, RpcError>;
    async fn request(&self, request: AppsRequestRequest) -> Result<AppsRequestResponse, RpcError>;
    async fn detach(&self, request: AppsDetachRequest) -> Result<(), RpcError>;
}

/// 生产实现：所有调用都包外层超时；响应按 facade 契约类型解码。
pub struct AcpAppsRpc {
    client: Arc<AcpTuiClient>,
}

impl AcpAppsRpc {
    #[must_use]
    pub fn new(client: Arc<AcpTuiClient>) -> Self {
        Self { client }
    }

    async fn call<T: DeserializeOwned>(&self, method: &str, params: Value) -> Result<T, RpcError> {
        let result =
            tokio::time::timeout(ACP_TIMEOUT, self.client.send_raw_request(method, params)).await;
        match result {
            Err(_) => Err(RpcError::new("apps_rpc_timeout")),
            Ok(Err(error)) => Err(RpcError::from_acp(&error)),
            Ok(Ok(value)) => {
                serde_json::from_value(value).map_err(|_| RpcError::new("apps_rpc_bad_response"))
            }
        }
    }
}

#[async_trait]
impl AppsRpc for AcpAppsRpc {
    fn current_session_id(&self) -> Option<String> {
        self.client.current_session_id()
    }

    async fn list(&self, session_id: &str) -> Result<AppsListResponse, RpcError> {
        let params = serde_json::to_value(AppsListRequest {
            session_id: session_id.to_string(),
        })
        .map_err(|_| RpcError::new("apps_rpc_encode"))?;
        self.call("peri/mcp/apps/list", params).await
    }

    async fn attach(&self, request: AppsAttachRequest) -> Result<AppsAttachResponse, RpcError> {
        let params = serde_json::to_value(request).map_err(|_| RpcError::new("apps_rpc_encode"))?;
        self.call("peri/mcp/apps/attach", params).await
    }

    async fn request(&self, request: AppsRequestRequest) -> Result<AppsRequestResponse, RpcError> {
        let params = serde_json::to_value(request).map_err(|_| RpcError::new("apps_rpc_encode"))?;
        self.call("peri/mcp/apps/request", params).await
    }

    async fn detach(&self, request: AppsDetachRequest) -> Result<(), RpcError> {
        let params = serde_json::to_value(request).map_err(|_| RpcError::new("apps_rpc_encode"))?;
        let _: Value = self.call("peri/mcp/apps/detach", params).await?;
        Ok(())
    }
}

// ── 命令通道 ────────────────────────────────────────────────────────────────

/// 运行时命令（UI/面板 → 运行时任务）。
#[derive(Debug, Clone, PartialEq)]
pub enum RuntimeCommand {
    Open {
        app_id: String,
    },
    CloseView {
        view_id: String,
    },
    CloseAll,
    /// 面板画布左键点击（CSS 视口坐标）。
    Click {
        view_id: String,
        point: ViewportPoint,
    },
}

static COMMAND_TX: Mutex<Option<mpsc::UnboundedSender<RuntimeCommand>>> = Mutex::new(None);

fn install_command_tx(tx: mpsc::UnboundedSender<RuntimeCommand>) {
    *COMMAND_TX
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(tx);
}

fn clear_command_tx() {
    *COMMAND_TX
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
}

/// 发送运行时命令；运行时未启动时返回 `false`（调用方据此保持零副作用）。
pub fn send_command(command: RuntimeCommand) -> bool {
    let sender = COMMAND_TX
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    sender.is_some_and(|tx| tx.send(command).is_ok())
}

/// 请求打开一个已发现的 App。
pub fn request_open_app(app_id: &str) -> bool {
    send_command(RuntimeCommand::Open {
        app_id: app_id.to_string(),
    })
}

/// 关闭单个视图（面板 `x`）。
pub fn request_close_view(view_id: &str) -> bool {
    send_command(RuntimeCommand::CloseView {
        view_id: view_id.to_string(),
    })
}

/// 关闭所有视图（面板关闭 / TUI 退出）。幂等；运行时未启动时无副作用。
pub fn request_close_all() {
    let _ = send_command(RuntimeCommand::CloseAll);
}

/// 转发一次画布点击；运行时未启动或无视图时返回 `false`（零网络）。
pub fn request_view_click(view_id: &str, point: ViewportPoint) -> bool {
    send_command(RuntimeCommand::Click {
        view_id: view_id.to_string(),
        point,
    })
}

// ── 句柄与启动 ──────────────────────────────────────────────────────────────

/// 运行时句柄：TUI 退出时 cancel + 有界 join（保证 DELETE 有机会发出）。
pub struct AppsRuntimeHandle {
    cancel: CancellationToken,
    join: JoinHandle<()>,
}

impl AppsRuntimeHandle {
    /// 请求关闭并等待运行时的清理路径（上限 5s，不阻塞退出流程）。
    pub async fn shutdown(self) {
        self.cancel.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(5), self.join).await;
    }
}

/// 开关/配置未就绪时返回 `None`：不 spawn、不安装命令通道。
///
/// 这是「关闭开关或无配置时零网络调用、零轮询」的唯一门控点。
pub fn spawn_if_enabled(
    client: Arc<AcpTuiClient>,
    shutdown: CancellationToken,
) -> Option<AppsRuntimeHandle> {
    spawn_with_availability(
        config::from_env(),
        Arc::new(AcpAppsRpc::new(client)),
        shutdown,
    )
}

/// 注入可用性与 fake RPC 的启动入口（测试用）。
pub fn spawn_with_availability(
    availability: AppsAvailability,
    rpc: Arc<dyn AppsRpc>,
    shutdown: CancellationToken,
) -> Option<AppsRuntimeHandle> {
    let AppsAvailability::Enabled(config) = availability else {
        config::warn_if_unconfigured(&availability);
        return None;
    };
    let api = match AppsViewClient::new(&config.view_url, config.token.clone()) {
        Ok(client) => Arc::new(client) as Arc<dyn ViewSessionApi>,
        Err(error) => {
            tracing::warn!(code = %error, "MCP Apps TUI view client construction failed; app views stay off");
            return None;
        }
    };
    Some(spawn_with_parts(config, rpc, api, shutdown))
}

/// 组装运行时任务（`api` 由测试替换为本地 stub）。
pub fn spawn_with_parts(
    config: AppsConfig,
    rpc: Arc<dyn AppsRpc>,
    api: Arc<dyn ViewSessionApi>,
    shutdown: CancellationToken,
) -> AppsRuntimeHandle {
    let (command_tx, command_rx) = mpsc::unbounded_channel();
    install_command_tx(command_tx);
    let cancel = shutdown.child_token();
    let runtime = Runtime {
        config,
        rpc,
        api,
        cancel: cancel.clone(),
        commands: command_rx,
        session_id: None,
        views: std::collections::HashMap::new(),
        policy: StreamPolicy::default(),
    };
    let join = tokio::spawn(runtime.run());
    AppsRuntimeHandle { cancel, join }
}

// ── 运行时主体 ──────────────────────────────────────────────────────────────

struct RunningView {
    cancel: CancellationToken,
    join: JoinHandle<()>,
}

struct Runtime {
    config: AppsConfig,
    rpc: Arc<dyn AppsRpc>,
    api: Arc<dyn ViewSessionApi>,
    cancel: CancellationToken,
    commands: mpsc::UnboundedReceiver<RuntimeCommand>,
    session_id: Option<String>,
    views: std::collections::HashMap<String, RunningView>,
    policy: StreamPolicy,
}

impl Runtime {
    async fn run(mut self) {
        state::set_runtime_state(AppsRuntimeState::Active);
        let mut ticker = tokio::time::interval(self.config.discovery_interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                biased;
                _ = self.cancel.cancelled() => break,
                Some(command) = self.commands.recv() => self.handle_command(command).await,
                _ = ticker.tick() => self.refresh_discovery().await,
            }
            self.reap_finished().await;
        }
        self.shutdown_views().await;
        state::set_runtime_state(AppsRuntimeState::Inactive);
        clear_command_tx();
    }

    /// 会话切换：先关闭旧视图（DELETE），再清空 descriptor。
    async fn refresh_discovery(&mut self) {
        let Some(session_id) = self.rpc.current_session_id() else {
            return;
        };
        if self.session_id.as_deref() != Some(session_id.as_str()) {
            self.shutdown_views().await;
            state::publish_descriptors(Vec::new());
            self.session_id = Some(session_id.clone());
        }
        match self.rpc.list(&session_id).await {
            Ok(listed) => {
                let entries = listed
                    .descriptors
                    .iter()
                    .map(|descriptor| AppEntry::from_descriptor(&session_id, descriptor))
                    .collect::<Vec<_>>();
                state::publish_descriptors(entries);
            }
            Err(error) => state::publish_discovery_error(error.code()),
        }
    }

    async fn handle_command(&mut self, command: RuntimeCommand) {
        match command {
            RuntimeCommand::Open { app_id } => self.open_app(&app_id).await,
            RuntimeCommand::CloseView { view_id } => {
                self.close_view(&view_id).await;
                state::remove_view(&view_id);
            }
            RuntimeCommand::CloseAll => self.shutdown_views().await,
            RuntimeCommand::Click { view_id, point } => self.click_view(view_id, point),
        }
    }

    /// 画布点击：只转发到该视图自己的网关会话；错误码写入视图状态。
    ///
    /// 独立 task，避免一次 CDP 调用阻塞发现/关闭命令；不加日志（坐标不落日志）。
    fn click_view(&self, view_id: String, point: ViewportPoint) {
        let Some(view_session_id) = state::APPS_STATE
            .state()
            .read()
            .view(&view_id)
            .and_then(|view| view.view_session_id.clone())
        else {
            return;
        };
        let api = Arc::clone(&self.api);
        let cancel = self.cancel.child_token();
        tokio::spawn(async move {
            let result = tokio::select! {
                biased;
                _ = cancel.cancelled() => return,
                result = api.click(&view_session_id, point) => result,
            };
            match result {
                Ok(()) => state::update_view(&view_id, |view| {
                    let changed = view.last_error.is_some();
                    view.last_error = None;
                    changed
                }),
                Err(error) => state::update_view(&view_id, |view| {
                    view.last_error = Some(error.code().to_string());
                    true
                }),
            }
        });
    }

    async fn open_app(&mut self, app_id: &str) {
        let Some(session_id) = self.rpc.current_session_id() else {
            state::set_open_error(Some("apps_no_session".to_string()));
            return;
        };
        // 同一 App 已有存活视图：只切活动视图，不重复 attach。
        {
            let current = state::APPS_STATE.state().read().clone();
            if let Some(existing) = current.live_view_for_app(app_id) {
                let view_id = existing.view_id.clone();
                drop(current);
                state::set_active_view(&view_id);
                state::set_open_error(None);
                return;
            }
            let live = current
                .views
                .iter()
                .filter(|view| !view.phase.is_terminal())
                .count();
            if live >= state::MAX_CONCURRENT_VIEWS {
                drop(current);
                state::set_open_error(Some("apps_view_limit".to_string()));
                return;
            }
        }
        let Some(entry) = state::descriptor_by_app_id(app_id) else {
            state::set_open_error(Some("apps_app_not_found".to_string()));
            return;
        };
        let view_id = uuid::Uuid::now_v7().simple().to_string();
        state::insert_view(AppView::new(&view_id, entry.clone()));
        state::set_open_error(None);

        let view_cancel = self.cancel.child_token();
        let context = ViewContext {
            rpc: Arc::clone(&self.rpc),
            api: Arc::clone(&self.api),
            session_id,
            entry,
            view_id: view_id.clone(),
            cancel: view_cancel.clone(),
            policy: self.policy,
        };
        let join = tokio::spawn(run_view(context));
        self.views.insert(
            view_id,
            RunningView {
                cancel: view_cancel,
                join,
            },
        );
    }

    async fn close_view(&mut self, view_id: &str) {
        if let Some(view) = self.views.remove(view_id) {
            view.cancel.cancel();
            let joined = tokio::time::timeout(CLEANUP_TIMEOUT, view.join).await;
            // 任务 panic/超时退出时 run_view 的 DELETE 可能没跑到：幂等兜底。
            if !matches!(joined, Ok(Ok(()))) {
                self.delete_session_best_effort(view_id).await;
            }
        }
    }

    /// 兜底 DELETE：任务正常结束时通常已删过（404 视为成功）。
    async fn delete_session_best_effort(&self, view_id: &str) {
        let session_id = state::APPS_STATE
            .state()
            .read()
            .view(view_id)
            .and_then(|view| view.view_session_id.clone());
        if let Some(session_id) = session_id {
            let _ =
                tokio::time::timeout(CLEANUP_TIMEOUT, self.api.delete_session(&session_id)).await;
        }
    }

    /// 取消所有视图并等待清理（DELETE）完成。
    async fn shutdown_views(&mut self) {
        let views = std::mem::take(&mut self.views);
        for view in views.values() {
            view.cancel.cancel();
        }
        for (view_id, view) in views {
            let joined = tokio::time::timeout(CLEANUP_TIMEOUT, view.join).await;
            if !matches!(joined, Ok(Ok(()))) {
                self.delete_session_best_effort(&view_id).await;
            }
            state::remove_view(&view_id);
        }
    }

    async fn reap_finished(&mut self) {
        let finished = self
            .views
            .iter()
            .filter(|(_, view)| view.join.is_finished())
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        for view_id in finished {
            if let Some(view) = self.views.remove(&view_id) {
                let _ = view.join.await;
            }
        }
    }
}

// ── 单视图生命周期 ──────────────────────────────────────────────────────────

/// SSE 退避策略（测试注入极小值保持确定性）。
#[derive(Debug, Clone, Copy)]
pub struct StreamPolicy {
    pub max_attempts: u32,
    pub initial_backoff: Duration,
    pub max_backoff: Duration,
}

impl Default for StreamPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 20,
            initial_backoff: Duration::from_millis(500),
            max_backoff: Duration::from_secs(8),
        }
    }
}

impl StreamPolicy {
    fn backoff(&self, attempt: u32) -> Duration {
        let exponent = attempt.saturating_sub(1).min(16);
        let factor = 1u64 << exponent;
        self.initial_backoff
            .saturating_mul(u32::try_from(factor).unwrap_or(u32::MAX))
            .min(self.max_backoff)
    }
}

pub fn runtime_installed() -> bool {
    COMMAND_TX
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .is_some()
}

#[cfg(test)]
#[path = "runtime_test.rs"]
mod tests;
