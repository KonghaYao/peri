use crate::acp_client::AcpTuiClient;
use crate::application::operations::{
    CompletionGate, OperationOutcome, OperationScope, OperationTicket, OutcomeUnknownReason,
};
use crate::kit::atoms::{ACP_CLIENT_HANDLE, PluginSummary};
use crate::kit::panel_mouse::{ListLayout, hit_item, is_scrollbar_column};
use ratatui_kit::crossterm::event::{Event, KeyCode, KeyEventKind};
use ratatui_kit::prelude::State;
use ratatui_kit::ratatui::layout::Rect;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};

const OPERATION_TIMEOUT: Duration = Duration::from_secs(10);

pub(super) fn detail_action_index(
    event: &Event,
    area: Option<Rect>,
    plugin: &PluginSummary,
    selected: usize,
) -> Option<usize> {
    match event {
        Event::Key(key) if key.kind == KeyEventKind::Press && key.code == KeyCode::Enter => {
            Some(selected)
        }
        Event::Mouse(mouse) => {
            let area = area?;
            if is_scrollbar_column(mouse, area) {
                return None;
            }
            hit_item(
                mouse,
                area,
                ListLayout {
                    header_rows: if plugin.load_error.is_some() { 18 } else { 16 },
                    item_rows: 1,
                    footer_rows: 0,
                    visible_items: 4,
                    scroll_start: 0,
                    item_count: 4,
                },
            )
        }
        _ => None,
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct PluginOperation {
    pub action: &'static str,
    pub method: &'static str,
    pub params: Value,
}

impl PluginOperation {
    pub fn installed(action: &str, plugin: &PluginSummary) -> Result<Self, String> {
        let plugin_id = if plugin.marketplace.is_empty() {
            plugin.name.clone()
        } else {
            format!("{}@{}", plugin.name, plugin.marketplace)
        };
        let (action, method) = match action {
            "enable" => ("enable", "plugin/toggle"),
            "disable" => ("disable", "plugin/toggle"),
            "uninstall" => ("uninstall", "plugin/uninstall"),
            "update" => ("update", "plugin/update"),
            _ => return Err(format!("Unsupported plugin action: {action}")),
        };
        let scope = plugin.install_scope.as_deref();
        if !matches!(scope, Some("user" | "project" | "local"))
            || plugin.toggle_supported != Some(true)
        {
            return Err(plugin
                .management_error
                .as_ref()
                .filter(|error| !error.trim().is_empty())
                .cloned()
                .unwrap_or_else(|| crate::i18n::tr("panel-plugin-management-unsupported")));
        }
        let mut params = json!({"pluginId": plugin_id, "scope": scope});
        if method == "plugin/toggle" {
            params["enable"] = json!(action == "enable");
        }
        Ok(Self {
            action,
            method,
            params,
        })
    }

    pub fn marketplace_add(source: String) -> Self {
        Self {
            action: "add_marketplace",
            method: "marketplace/add",
            params: json!({"source": source}),
        }
    }

    pub fn marketplace_remove(name: String) -> Self {
        Self {
            action: "delete_marketplace",
            method: "marketplace/remove",
            params: json!({"name": name}),
        }
    }

    pub fn marketplace_refresh(name: String) -> Self {
        Self {
            action: "refresh_marketplace",
            method: "marketplace/refresh",
            params: json!({"name": name}),
        }
    }

    pub fn install(name: String, marketplace: String, scope: &str) -> Self {
        Self {
            action: "install",
            method: "plugin/install",
            params: json!({"name": name, "marketplace": marketplace, "scope": scope}),
        }
    }
}

/// 插件操作的面板状态。
///
/// 在途身份（作用域、代次、取消令牌）与过期拒收由 `application::operations` 的
/// 完成门持有；这里只保留插件自己的在途动作标签、待确认目标与反馈文案。
#[derive(Default)]
pub(super) struct OperationState {
    gate: CompletionGate,
    /// 在途动作标签：footer 文案与 Esc 判定用。原语不认识插件动作。
    pending_action: Option<&'static str>,
    pub confirmation: Option<PluginOperation>,
    pub error: Option<String>,
}

/// 完成词汇 → 面板反馈文案。文案属视图层，原语只给枚举。
fn outcome_message(outcome: &OperationOutcome) -> Option<String> {
    match outcome {
        OperationOutcome::Succeeded => None,
        OperationOutcome::Failed { reason } => Some(reason.clone()),
        OperationOutcome::OutcomeUnknown { reason } => Some(match reason {
            OutcomeUnknownReason::WaitCancelled => {
                crate::i18n::tr("panel-plugin-operation-wait-cancelled")
            }
            OutcomeUnknownReason::ObserverGone => {
                crate::i18n::tr("panel-plugin-operation-session-changed")
            }
            OutcomeUnknownReason::TimedOut => crate::i18n::tr("panel-plugin-operation-timed-out"),
        }),
    }
}

impl OperationState {
    /// Esc：停止本地等待。结果记为「未确认」——远端可能仍在执行，
    /// 不能显示为成功，也不能显示为已撤销。
    pub fn cancel_wait(&mut self) -> bool {
        let Some(action) = self.pending_action else {
            return false;
        };
        self.gate.cancel();
        self.pending_action = None;
        self.error = outcome_message(&OperationOutcome::OutcomeUnknown {
            reason: OutcomeUnknownReason::WaitCancelled,
        });
        tracing::warn!(session_id = %self.gate.scope().session_id(), action,
            "Plugin client wait cancelled; server outcome unknown");
        true
    }

    pub fn reset_session(&mut self, scope: OperationScope) {
        // 只认作用域变化：同作用域重复投影（每帧 effect）不得打断在途操作。
        if self.gate.reset_scope(scope) {
            self.pending_action = None;
            self.confirmation = None;
            self.error = None;
        }
    }

    /// 是否有在途操作：与完成门同源，不另存一份在途标志。
    pub fn is_pending(&self) -> bool {
        self.gate.is_pending()
    }

    pub fn pending_action(&self) -> Option<&'static str> {
        debug_assert_eq!(
            self.pending_action.is_some(),
            self.gate.is_pending(),
            "在途动作标签与完成门必须同生共死"
        );
        self.pending_action
    }

    pub fn begin(&mut self, operation: PluginOperation, scope: OperationScope) -> OperationTicket {
        self.error = None;
        self.confirmation = None;
        let ticket = self.gate.begin(scope);
        self.pending_action = Some(operation.action);
        ticket
    }

    /// 落地一次完成：只有通过完成门的结果才能改写面板状态，晚到的旧结果返回 false。
    pub fn complete(&mut self, ticket: &OperationTicket, outcome: OperationOutcome) -> bool {
        if !self.gate.accept(ticket) {
            return false;
        }
        self.pending_action = None;
        self.error = outcome_message(&outcome);
        true
    }
}

pub(super) fn handle_pending_event(event: &Event, state: State<OperationState>) -> bool {
    let pending = state.read().is_pending();
    if pending
        && matches!(event, Event::Key(key) if key.kind == KeyEventKind::Press && key.code == KeyCode::Esc)
    {
        state.write().cancel_wait();
    }
    pending
}

pub(super) fn dispatch(operation: PluginOperation, state: State<OperationState>) {
    let ticket = state
        .write()
        .begin(operation.clone(), super::current_scope());
    drop(launch(
        ticket,
        operation,
        ACP_CLIENT_HANDLE.get().cloned(),
        move |ticket, outcome| {
            if let Some(mut state) = state.try_write() {
                state.complete(ticket, outcome);
                None
            } else {
                Some(outcome)
            }
        },
    ));
}

pub(super) fn launch(
    ticket: OperationTicket,
    operation: PluginOperation,
    client: Option<Arc<AcpTuiClient>>,
    complete: impl Fn(&OperationTicket, OperationOutcome) -> Option<OperationOutcome> + Send + 'static,
) -> tokio::task::JoinHandle<()> {
    let identity = client.as_ref().and_then(|client| {
        client
            .stable_session_identity()
            .filter(|identity| identity.0 == ticket.scope().session_id())
    });
    tokio::spawn(async move {
        let mut outcome = if let Some(client) = client.as_ref() {
            let mut params = operation.params.clone();
            params["sessionId"] = json!(ticket.scope().session_id());
            match tokio::select! {
                biased;
                _ = ticket.token().cancelled() => return,
                result = peri_time::timeout(OPERATION_TIMEOUT, async {
                    let identity = identity.as_ref().ok_or_else(|| {
                        "ACP session not available for plugin operation".to_string()
                    })?;
                    client.send_session_request(identity, operation.method, params)
                        .await.map_err(|error| error.to_string())
                }) => result,
            } {
                // 响应形状是插件自己的业务约定：success 非 true 一律按失败。
                Ok(Ok(value)) if value.get("success").and_then(Value::as_bool) == Some(true) => {
                    OperationOutcome::Succeeded
                }
                Ok(Ok(value)) => OperationOutcome::Failed {
                    reason: format!("Invalid plugin operation response: {value}"),
                },
                Ok(Err(error)) => OperationOutcome::Failed { reason: error },
                // 客户端等待结束不代表服务端没执行：这是结果未确认，不是失败。
                Err(_) => OperationOutcome::OutcomeUnknown {
                    reason: OutcomeUnknownReason::TimedOut,
                },
            }
        } else {
            OperationOutcome::Failed {
                reason: "ACP client not available".into(),
            }
        };
        if let Some(client) = client.as_ref()
            && identity.is_some()
            && client.stable_session_identity() != identity
        {
            if let OperationOutcome::Failed { reason } = &outcome {
                tracing::error!(session_id = %ticket.scope().session_id(), method = operation.method,
                    %reason, "Stale plugin operation failed");
            }
            // 会话身份已替换：已发出的变更失去归属，既不称成功也不称失败。
            outcome = OperationOutcome::OutcomeUnknown {
                reason: OutcomeUnknownReason::ObserverGone,
            };
        }
        // 插件业务特有副作用：marketplace 目录变更成功后刷新本地缓存。
        if matches!(outcome, OperationOutcome::Succeeded)
            && matches!(
                operation.method,
                "marketplace/add" | "marketplace/remove" | "marketplace/refresh"
            )
        {
            tokio::select! {
                biased;
                _ = ticket.token().cancelled() => return,
                refresh = peri_time::timeout(OPERATION_TIMEOUT, tokio::task::spawn_blocking(|| {
                    super::data::refresh_discover_cache();
                    super::data::refresh_marketplace_cache();
                })) => match refresh {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => outcome = OperationOutcome::Failed { reason: format!("Marketplace cache refresh failed: {error}") },
                    Err(error) => outcome = OperationOutcome::Failed { reason: format!("Marketplace operation succeeded; cache refresh timed out: {error}") },
                }
            }
        }
        if let OperationOutcome::Failed { reason } = &outcome {
            tracing::error!(session_id = %ticket.scope().session_id(), method = operation.method, %reason, "Plugin operation failed");
        }
        loop {
            // 作用域已变：本次结果不再属于当前面板，静默丢弃。
            if ticket.is_cancelled() || ticket.scope() != &super::current_scope() {
                return;
            }
            if let Some(client) = client.as_ref()
                && identity.is_some()
                && client.stable_session_identity() != identity
            {
                let reason = crate::i18n::tr("panel-plugin-operation-session-changed");
                tracing::warn!(session_id = %ticket.scope().session_id(), method = operation.method,
                    %reason, "Discarding stale plugin operation result");
                outcome = OperationOutcome::OutcomeUnknown {
                    reason: OutcomeUnknownReason::ObserverGone,
                };
            }
            let Some(unconsumed) = complete(&ticket, outcome) else {
                return;
            };
            outcome = unconsumed;
            tokio::select! {
                biased;
                _ = ticket.token().cancelled() => return,
                _ = peri_time::sleep(Duration::from_millis(1)) => {}
            }
        }
    })
}

pub(super) fn installed_action(
    action: &str,
    plugin: &PluginSummary,
    state: State<OperationState>,
    confirm_action: State<Option<String>>,
    detail_plugin_idx: State<Option<usize>>,
) {
    if action == "back" {
        *detail_plugin_idx.write() = None;
        return;
    }
    match PluginOperation::installed(action, plugin) {
        Ok(operation) => {
            if action == "uninstall" {
                state.write().confirmation = Some(operation);
                *confirm_action.write() = Some("uninstall".into());
            } else {
                dispatch(operation, state);
                *detail_plugin_idx.write() = None;
            }
        }
        Err(error) => {
            tracing::error!(plugin_name = %plugin.name, marketplace = %plugin.marketplace,
                install_scope = ?plugin.install_scope, toggle_supported = ?plugin.toggle_supported,
                action, %error, "Plugin operation unsupported");
            state.write().error = Some(error);
        }
    }
}

pub(super) fn confirm_uninstall(
    state: State<OperationState>,
    confirm_action: State<Option<String>>,
    detail_plugin_idx: State<Option<usize>>,
) {
    let operation = state.write().confirmation.take();
    *confirm_action.write() = None;
    *detail_plugin_idx.write() = None;
    if let Some(operation) = operation {
        dispatch(operation, state);
    }
}

pub(super) fn confirm_marketplace_remove(
    name: String,
    state: State<OperationState>,
    confirm_action: State<Option<String>>,
    operation_loading: State<Option<String>>,
) {
    *confirm_action.write() = None;
    *operation_loading.write() = None;
    dispatch(PluginOperation::marketplace_remove(name), state);
}

#[cfg(test)]
#[path = "operation_test.rs"]
pub(super) mod tests;
