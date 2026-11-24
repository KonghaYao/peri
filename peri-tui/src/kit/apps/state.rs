//! MCP App 视图状态：发现结果、多个 view 会话、最新帧与面板状态机。
//!
//! 设计约束：
//! - **多视图并存**（验收 A6）：`views` 按本地 `view_id` 区分，反向完成的
//!   回执不会串台；面板只渲染 `active_view`，但每个 view 的帧/错误独立保存。
//! - **单写多读**：后台 task 通过 [`publish_view`](crate::kit::apps::state::publish_view)
//!   之类的辅助函数写全局 atom，渲染 body 只读（TUI-RENDER-001）。
//! - **内存有界**：view 数 ≤ [`MAX_CONCURRENT_VIEWS`]，帧只保留最新一张；
//!   App HTML/base64 不进入本状态（只保留解码后的图像）。
//! - 状态文本只含错误码，不含网关 message 或 URL。

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use image::DynamicImage;
use ratatui_kit::prelude::Atom as AtomStatic;

use crate::kit::atoms::RENDER_HEARTBEAT;

use super::view_client::ViewEvent;

/// 并发 view 上限：与网关 `MAX_SESSIONS`（4）对齐，超出时不再 attach。
pub const MAX_CONCURRENT_VIEWS: usize = 4;

/// 发现结果中的一个 App（来自 `apps/list` 的 descriptor 投影）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppEntry {
    /// ACP 会话 ID（跨会话不匹配，避免历史工具卡误命中）。
    pub session_id: String,
    pub app_id: String,
    pub server_id: String,
    pub resource_uri: String,
    pub instantiating_tool: String,
    /// 模型 tool-call ID；与 TUI 工具卡 `tool_id` 同源（A1 匹配依据）。
    pub tool_call_id: Option<String>,
}

impl AppEntry {
    /// 从 facade descriptor 投影；只保留 UI 需要的字段。
    #[must_use]
    pub fn from_descriptor(
        session_id: &str,
        descriptor: &peri_acp_types::mcp_apps_facade::AppDescriptor,
    ) -> Self {
        Self {
            session_id: session_id.to_string(),
            app_id: descriptor.app_id.clone(),
            server_id: descriptor.server_id.clone(),
            resource_uri: descriptor.resource_uri.clone(),
            instantiating_tool: descriptor.instantiating_tool.clone(),
            tool_call_id: descriptor
                .model
                .tool_call_id
                .as_ref()
                .map(|id| id.as_str().to_string()),
        }
    }

    /// 用户可见的短名：优先实例化工具名（demo 的 `get-time`），再退到 server。
    #[must_use]
    pub fn display_name(&self) -> String {
        if !self.instantiating_tool.is_empty() {
            self.instantiating_tool.clone()
        } else if !self.server_id.is_empty() {
            self.server_id.clone()
        } else {
            self.app_id.clone()
        }
    }
}

/// 一个 view 会话的生命周期阶段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewPhase {
    /// 正在 attach / 读取 HTML / 建 view session / 等首帧。
    Starting,
    /// 网关已 ready（或已收到帧）。
    Ready,
    /// SSE 断线，按退避重连中。
    Reconnecting { attempt: u32 },
    /// 网关报告 failed（`code` 只保留错误码）。
    Failed { code: String },
    /// 会话已关闭（远端 `closed` 事件或本地关闭）。
    Closed { reason: Option<String> },
    /// 宿主侧失败（attach / resources/read / 建会话错误）。
    LocalError { code: String },
}

impl ViewPhase {
    /// i18n key（面板翻译；状态机本身不依赖 i18n）。
    #[must_use]
    pub fn label_key(&self) -> &'static str {
        match self {
            Self::Starting => "apps-phase-starting",
            Self::Ready => "apps-phase-ready",
            Self::Reconnecting { .. } => "apps-phase-reconnecting",
            Self::Failed { .. } | Self::LocalError { .. } => "apps-phase-failed",
            Self::Closed { .. } => "apps-phase-closed",
        }
    }

    /// 错误码（状态展示用，不含 message）。
    #[must_use]
    pub fn error_code(&self) -> Option<&str> {
        match self {
            Self::Failed { code } | Self::LocalError { code } => Some(code),
            Self::Starting | Self::Ready | Self::Reconnecting { .. } | Self::Closed { .. } => None,
        }
    }

    /// 终态：关闭或失败后不再接受帧。
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Failed { .. } | Self::Closed { .. } | Self::LocalError { .. }
        )
    }
}

/// 最新一帧（解码后的图像 + 尺寸/序号）。
#[derive(Clone)]
pub struct FrameImage {
    pub seq: u64,
    pub width: u32,
    pub height: u32,
    pub viewport_width: u32,
    pub viewport_height: u32,
    pub image: Arc<DynamicImage>,
}

impl std::fmt::Debug for FrameImage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FrameImage")
            .field("seq", &self.seq)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("viewport_width", &self.viewport_width)
            .field("viewport_height", &self.viewport_height)
            .finish_non_exhaustive()
    }
}

/// 一个已打开（或正在打开）的 App 视图。
#[derive(Debug, Clone)]
pub struct AppView {
    /// TUI 本地视图身份（uuid）；与网关 sessionId / attachmentId 分离。
    pub view_id: String,
    pub app: AppEntry,
    pub attachment_id: Option<String>,
    pub epoch: Option<u64>,
    /// `apps/attach` 快照里的 UI 资源引用（正文经 `resources/read` 另行读取）。
    pub ui_resource_uri: Option<String>,
    pub ui_resource_mime: Option<String>,
    /// 下一个可用 `apps/request` 序号（响应里的 `nextSeq` 推进）。
    pub next_seq: u64,
    /// 网关 view session ID。
    pub view_session_id: Option<String>,
    /// 网关返回的事件路径（相对或绝对）。
    pub events_url: Option<String>,
    pub phase: ViewPhase,
    pub frame: Option<FrameImage>,
    /// 网关未回答的 App 请求数（回填后回落；面板展示）。
    pub pending_requests: usize,
    /// 宿主侧最后一次失败的码（面板展示；状态 phase 可能仍是 Ready）。
    pub last_error: Option<String>,
}

impl AppView {
    #[must_use]
    pub fn new(view_id: impl Into<String>, app: AppEntry) -> Self {
        Self {
            view_id: view_id.into(),
            app,
            attachment_id: None,
            epoch: None,
            ui_resource_uri: None,
            ui_resource_mime: None,
            next_seq: 0,
            view_session_id: None,
            events_url: None,
            phase: ViewPhase::Starting,
            frame: None,
            pending_requests: 0,
            last_error: None,
        }
    }

    /// 应用网关状态快照（`meta` / `state`）。
    ///
    /// 终态不被非终态覆盖；`starting` 不作为已 ready 视图的回退。
    pub fn apply_status(&mut self, state: &str, code: Option<&str>) -> bool {
        if self.phase.is_terminal() {
            return false;
        }
        let next = match state {
            "failed" => {
                let code = code.unwrap_or("view_failed").to_string();
                self.last_error = Some(code.clone());
                ViewPhase::Failed { code }
            }
            "ready" => ViewPhase::Ready,
            // 已失败/已本地失败的视图保留失败码：网关随后发的 closed 只是清理事实。
            "closed" if self.phase.is_terminal() => return false,
            "closed" => ViewPhase::Closed { reason: None },
            // `starting` 与未知状态：只在未 ready 时保留 Starting。
            _ => match self.phase {
                ViewPhase::Ready | ViewPhase::Reconnecting { .. } => return false,
                _ => ViewPhase::Starting,
            },
        };
        if self.phase == next {
            return false;
        }
        self.phase = next;
        true
    }

    /// 应用一帧；乱序/重复序号被丢弃。
    pub fn apply_frame(&mut self, frame: FrameImage) -> bool {
        if self.phase.is_terminal() {
            return false;
        }
        if self
            .frame
            .as_ref()
            .is_some_and(|current| frame.seq <= current.seq)
        {
            return false;
        }
        if matches!(
            self.phase,
            ViewPhase::Starting | ViewPhase::Reconnecting { .. }
        ) {
            self.phase = ViewPhase::Ready;
        }
        self.frame = Some(frame);
        true
    }

    /// 应用解码后的业务事件；返回状态是否变化（用于决定是否发布）。
    pub fn apply_event(&mut self, event: &ViewEvent) -> bool {
        match event {
            ViewEvent::Meta(status) | ViewEvent::Status(status) => {
                self.apply_status(&status.state, status.code.as_deref())
            }
            ViewEvent::AppRequest(_) => {
                self.pending_requests = self.pending_requests.saturating_add(1);
                true
            }
            ViewEvent::Closed { reason } => {
                if self.phase.is_terminal() {
                    return false;
                }
                self.phase = ViewPhase::Closed {
                    reason: reason.clone(),
                };
                true
            }
            // 帧由 runtime 解码后经 `apply_frame` 落地（解码在阻塞线程）。
            ViewEvent::Frame(_) => false,
        }
    }
}

/// 运行时是否已启动（面板据此给出「未启用」提示）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AppsRuntimeState {
    /// 开关关闭或缺少网关配置：零网络、零轮询。
    #[default]
    Inactive,
    /// `apps/list` 轮询已启动（是否已拿到会话取决于会话是否就绪）。
    Active,
}

/// 全局 Apps 状态。
#[derive(Debug, Clone, Default)]
pub struct AppsState {
    pub runtime: AppsRuntimeState,
    pub descriptors: Vec<AppEntry>,
    pub views: Vec<AppView>,
    /// 面板当前渲染的 view（最新打开的视图优先）。
    pub active_view: Option<String>,
    /// 最近一次发现失败的错误码。
    pub discovery_error: Option<String>,
    /// 最近一次打开失败的错误码（面板展示）。
    pub open_error: Option<String>,
    /// 本地视图序号（生成稳定 view_id）。
    pub next_view_serial: u64,
}

impl AppsState {
    #[must_use]
    pub fn active(&self) -> Option<&AppView> {
        let id = self.active_view.as_ref()?;
        self.views.iter().find(|view| &view.view_id == id)
    }

    #[must_use]
    pub fn view(&self, view_id: &str) -> Option<&AppView> {
        self.views.iter().find(|view| view.view_id == view_id)
    }

    pub fn view_mut(&mut self, view_id: &str) -> Option<&mut AppView> {
        self.views.iter_mut().find(|view| view.view_id == view_id)
    }

    /// 同一 App 的现存视图（去重：再次点击同一工具卡只切换活动视图）。
    #[must_use]
    pub fn live_view_for_app(&self, app_id: &str) -> Option<&AppView> {
        self.views.iter().find(|view| {
            view.app.app_id == app_id && !matches!(view.phase, ViewPhase::Closed { .. })
        })
    }
}

/// 全局状态 atom；后台 task 与渲染共享。
pub static APPS_STATE: AtomStatic<AppsState> = AtomStatic::new(AppsState::default);

/// descriptor 集合版本：工具卡渲染缓存据此失效（发现结果变化时 +1）。
static REGISTRY_REVISION: AtomicU64 = AtomicU64::new(0);

/// 供 [`crate::kit::entry_render_cache`] 在渲染路径读取（不触发订阅）。
#[must_use]
pub fn registry_revision() -> u64 {
    REGISTRY_REVISION.load(Ordering::Acquire)
}

/// 发现结果变化：版本 +1，并推进结构纪元。
///
/// 工具卡/折叠组头的「打开应用」动作来自后台轮询的 `apps/list`；轮询完成时
/// ACP 事件流通常已静止，只改版本不会让消息区重算。结构纪元同时负责：
/// 唤醒订阅者（消息区订阅它）与让 transcript 重新派生（缓存失效）。
fn bump_registry_revision() {
    REGISTRY_REVISION.fetch_add(1, Ordering::AcqRel);
    let epoch = crate::kit::atoms::STRUCTURAL_EPOCH.get().saturating_add(1);
    crate::kit::atoms::STRUCTURAL_EPOCH.set(epoch);
}

/// 全局状态的写入串行化。
///
/// `AtomState` 的 read-clone-set 不是原子 RMW：两个 view task 同时更新不同
/// view 会互相覆盖（A6 并发视图）。所有写路径经本锁 + `mutate` 串行化；
/// 渲染路径只做 read（可能看到旧值，但不会看到半更新）。
static STATE_WRITE_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

/// 原子读-改-写：`update` 返回 `(changed, result)`；无变化不发布。
fn mutate<R>(update: impl FnOnce(&mut AppsState) -> (bool, R)) -> R {
    let _guard = STATE_WRITE_LOCK.lock();
    let mut state = { APPS_STATE.state().read().clone() };
    let (changed, result) = update(&mut state);
    if changed {
        APPS_STATE.state().set(state);
        RENDER_HEARTBEAT.set(RENDER_HEARTBEAT.get().wrapping_add(1));
    }
    result
}

/// 读-改-写全局状态；`update` 返回是否发生实际变化。
pub fn update_state(update: impl FnOnce(&mut AppsState) -> bool) {
    mutate(|state| (update(state), ()));
}

/// 整体替换状态（测试复位用）。
#[cfg(test)]
pub(crate) fn reset_for_test() {
    update_state(|state| {
        *state = AppsState::default();
        true
    });
    bump_registry_revision();
}

/// 发布发现结果；仅在内容变化时 bump 版本。
pub fn publish_descriptors(descriptors: Vec<AppEntry>) {
    let changed = mutate(|state| {
        if state.descriptors == descriptors && state.discovery_error.is_none() {
            return (false, false);
        }
        state.descriptors = descriptors;
        state.discovery_error = None;
        (true, true)
    });
    if changed {
        bump_registry_revision();
    }
}

/// 记录发现失败（只保留错误码），保留上一次成功的 descriptor 列表。
pub fn publish_discovery_error(code: impl Into<String>) {
    let code = code.into();
    update_state(|state| {
        if state.discovery_error.as_deref() == Some(code.as_str()) {
            return false;
        }
        state.discovery_error = Some(code);
        true
    });
}

/// 运行时状态切换。
pub fn set_runtime_state(runtime: AppsRuntimeState) {
    update_state(|state| {
        if state.runtime == runtime {
            return false;
        }
        state.runtime = runtime;
        true
    });
}

/// 登记新视图并设为活动视图。并发上限由 runtime 在 attach 前强制
/// （`MAX_CONCURRENT_VIEWS`）；这里不再静默淘汰，避免留下无人 DELETE 的会话。
pub fn insert_view(view: AppView) {
    update_state(|state| {
        state.views.retain(|item| item.view_id != view.view_id);
        state.active_view = Some(view.view_id.clone());
        state.views.push(view);
        true
    });
}

/// 移除视图；活动视图顺延到相邻项。
pub fn remove_view(view_id: &str) {
    update_state(|state| {
        let before = state.views.len();
        state.views.retain(|item| item.view_id != view_id);
        let mut changed = state.views.len() != before;
        if state.active_view.as_deref() == Some(view_id) {
            state.active_view = state.views.last().map(|item| item.view_id.clone());
            changed = true;
        }
        changed
    });
}

/// 更新单个视图；`update` 返回 `false` 时不发布。
pub fn update_view(view_id: &str, update: impl FnOnce(&mut AppView) -> bool) {
    mutate(|state| match state.view_mut(view_id) {
        Some(view) => (update(view), ()),
        None => (false, ()),
    });
}

/// 设置活动视图。
pub fn set_active_view(view_id: &str) {
    update_state(|state| {
        if state.active_view.as_deref() == Some(view_id)
            || !state.views.iter().any(|view| view.view_id == view_id)
        {
            return false;
        }
        state.active_view = Some(view_id.to_string());
        true
    });
}

/// 在视图列表内循环切换（`delta` = ±1）。
pub fn cycle_active_view(delta: i32) {
    update_state(|state| {
        if state.views.is_empty() {
            return state.active_view.take().is_some();
        }
        let current = state
            .active_view
            .as_ref()
            .and_then(|id| state.views.iter().position(|view| &view.view_id == id))
            .unwrap_or(0) as i64;
        let count = state.views.len() as i64;
        let next = (current + i64::from(delta)).rem_euclid(count) as usize;
        let next_id = state.views[next].view_id.clone();
        if state.active_view.as_deref() == Some(next_id.as_str()) {
            return false;
        }
        state.active_view = Some(next_id);
        true
    });
}

/// 设置打开失败错误码（面板展示；成功打开时清空）。
pub fn set_open_error(code: Option<String>) {
    update_state(|state| {
        if state.open_error == code {
            return false;
        }
        state.open_error = code;
        true
    });
}

/// 按模型 tool-call ID 找当前会话的 App（A1 工具卡入口）。
#[must_use]
pub fn app_for_tool_call(session_id: &str, tool_call_id: &str) -> Option<AppEntry> {
    if session_id.is_empty() || tool_call_id.is_empty() {
        return None;
    }
    APPS_STATE
        .state()
        .read()
        .descriptors
        .iter()
        .find(|entry| {
            entry.session_id == session_id && entry.tool_call_id.as_deref() == Some(tool_call_id)
        })
        .cloned()
}

/// 按 app_id 找 descriptor。
#[must_use]
pub fn descriptor_by_app_id(app_id: &str) -> Option<AppEntry> {
    APPS_STATE
        .state()
        .read()
        .descriptors
        .iter()
        .find(|entry| entry.app_id == app_id)
        .cloned()
}

#[cfg(test)]
#[path = "state_test.rs"]
mod tests;
