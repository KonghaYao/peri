use super::discover::SearchSession;
use crate::acp_client::AcpTuiClient;
use crate::kit::atoms::{ACP_CLIENT_HANDLE, PluginSummary};
use crate::kit::panel_mouse::{ListLayout, hit_item, is_scrollbar_column};
use ratatui_kit::crossterm::event::{Event, KeyCode, KeyEventKind};
use ratatui_kit::prelude::State;
use ratatui_kit::ratatui::layout::Rect;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

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

#[derive(Clone)]
pub(super) struct OperationTicket {
    pub session: SearchSession,
    operation: PluginOperation,
    generation: u64,
    cancelled: CancellationToken,
}

#[derive(Default)]
pub(super) struct OperationState {
    session: SearchSession,
    generation: u64,
    pending: Option<OperationTicket>,
    pub confirmation: Option<PluginOperation>,
    pub error: Option<String>,
}

impl Drop for OperationState {
    fn drop(&mut self) {
        self.cancel();
    }
}

impl OperationState {
    fn cancel(&mut self) {
        if let Some(ticket) = self.pending.take() {
            ticket.cancelled.cancel();
        }
    }

    pub fn cancel_wait(&mut self) -> bool {
        let Some(ticket) = self.pending.take() else {
            return false;
        };
        ticket.cancelled.cancel();
        self.error = Some(crate::i18n::tr("panel-plugin-operation-wait-cancelled"));
        tracing::warn!(session_id = %ticket.session.id, method = ticket.operation.method,
            "Plugin client wait cancelled; server outcome unknown");
        true
    }

    pub fn reset_session(&mut self, session: SearchSession) {
        if self.session != session {
            self.cancel();
            self.confirmation = None;
            self.error = None;
            self.session = session;
        }
    }

    pub fn pending_action(&self) -> Option<&'static str> {
        self.pending.as_ref().map(|ticket| ticket.operation.action)
    }

    pub fn begin(&mut self, operation: PluginOperation, session: SearchSession) -> OperationTicket {
        self.reset_session(session.clone());
        self.cancel();
        self.error = None;
        self.confirmation = None;
        self.generation = self
            .generation
            .checked_add(1)
            .expect("plugin operation generation exhausted");
        let ticket = OperationTicket {
            session,
            operation,
            generation: self.generation,
            cancelled: CancellationToken::new(),
        };
        self.pending = Some(ticket.clone());
        ticket
    }

    pub fn complete(&mut self, ticket: &OperationTicket, result: Result<(), String>) -> bool {
        if self.session != ticket.session
            || self.pending.as_ref().map(|pending| pending.generation) != Some(ticket.generation)
        {
            return false;
        }
        self.pending = None;
        self.error = result.err();
        true
    }
}

pub(super) fn handle_pending_event(event: &Event, state: State<OperationState>) -> bool {
    let pending = state.read().pending_action().is_some();
    if pending
        && matches!(event, Event::Key(key) if key.kind == KeyEventKind::Press && key.code == KeyCode::Esc)
    {
        state.write().cancel_wait();
    }
    pending
}

pub(super) fn dispatch(operation: PluginOperation, state: State<OperationState>) {
    let ticket = state.write().begin(operation, SearchSession::current());
    drop(launch(
        ticket,
        ACP_CLIENT_HANDLE.get().cloned(),
        move |ticket, result| {
            if let Some(mut state) = state.try_write() {
                state.complete(ticket, result);
                None
            } else {
                Some(result)
            }
        },
    ));
}

pub(super) fn launch(
    ticket: OperationTicket,
    client: Option<Arc<AcpTuiClient>>,
    complete: impl Fn(&OperationTicket, Result<(), String>) -> Option<Result<(), String>>
    + Send
    + 'static,
) -> tokio::task::JoinHandle<()> {
    let identity = client.as_ref().and_then(|client| {
        client
            .stable_session_identity()
            .filter(|identity| identity.0 == ticket.session.id)
    });
    tokio::spawn(async move {
        let mut result = if let Some(client) = client.as_ref() {
            let mut params = ticket.operation.params.clone();
            params["sessionId"] = json!(ticket.session.id);
            tokio::select! {
                biased;
                _ = ticket.cancelled.cancelled() => return,
                result = peri_time::timeout(OPERATION_TIMEOUT, async {
                    let identity = identity.as_ref().ok_or_else(|| {
                        "ACP session not available for plugin operation".to_string()
                    })?;
                    client.send_session_request(identity, ticket.operation.method, params)
                        .await.map_err(|error| error.to_string())
                }) => result.unwrap_or_else(|_| {
                    Err(crate::i18n::tr("panel-plugin-operation-timed-out"))
                }),
            }
            .and_then(|value| {
                if value.get("success").and_then(Value::as_bool) == Some(true) {
                    Ok(())
                } else {
                    Err(format!("Invalid plugin operation response: {value}"))
                }
            })
        } else {
            Err("ACP client not available".into())
        };
        if let Some(client) = client.as_ref()
            && identity.is_some()
            && client.stable_session_identity() != identity
        {
            if let Err(error) = &result {
                tracing::error!(session_id = %ticket.session.id, method = ticket.operation.method,
                    %error, "Stale plugin operation failed");
            }
            result = Err(crate::i18n::tr("panel-plugin-operation-session-changed"));
        }
        if result.is_ok()
            && matches!(
                ticket.operation.method,
                "marketplace/add" | "marketplace/remove" | "marketplace/refresh"
            )
        {
            tokio::select! {
                biased;
                _ = ticket.cancelled.cancelled() => return,
                refresh = peri_time::timeout(OPERATION_TIMEOUT, tokio::task::spawn_blocking(|| {
                    super::data::refresh_discover_cache();
                    super::data::refresh_marketplace_cache();
                })) => match refresh {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => result = Err(format!("Marketplace cache refresh failed: {error}")),
                    Err(error) => result = Err(format!("Marketplace operation succeeded; cache refresh timed out: {error}")),
                }
            }
        }
        if let Err(error) = &result {
            tracing::error!(session_id = %ticket.session.id, method = ticket.operation.method, %error, "Plugin operation failed");
        }
        loop {
            if ticket.cancelled.is_cancelled() || SearchSession::current() != ticket.session {
                return;
            }
            if let Some(client) = client.as_ref()
                && identity.is_some()
                && client.stable_session_identity() != identity
            {
                let error = crate::i18n::tr("panel-plugin-operation-session-changed");
                tracing::warn!(session_id = %ticket.session.id, method = ticket.operation.method,
                    %error, "Discarding stale plugin operation result");
                result = Err(error);
            }
            let Some(unconsumed) = complete(&ticket, result) else {
                return;
            };
            result = unconsumed;
            tokio::select! {
                biased;
                _ = ticket.cancelled.cancelled() => return,
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
