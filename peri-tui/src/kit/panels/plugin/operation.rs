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
    pub fn installed(action: &str, plugin: &PluginSummary) -> Option<Self> {
        let plugin_id = if plugin.marketplace.is_empty() {
            plugin.name.clone()
        } else {
            format!("{}@{}", plugin.name, plugin.marketplace)
        };
        let mut params = json!({"pluginId": plugin_id});
        let (action, method) = match action {
            "enable" => ("enable", "plugin/toggle"),
            "disable" => ("disable", "plugin/toggle"),
            "uninstall" => ("uninstall", "plugin/uninstall"),
            "update" => ("update", "plugin/update"),
            _ => return None,
        };
        if method == "plugin/toggle" {
            params["enable"] = json!(action == "enable");
            params["scope"] = json!(plugin.install_scope);
        }
        Some(Self {
            action,
            method,
            params,
        })
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
    tokio::spawn(async move {
        let mut result = if let Some(client) = client {
            let mut params = ticket.operation.params.clone();
            params["sessionId"] = json!(ticket.session.id);
            tokio::select! {
                biased;
                _ = ticket.cancelled.cancelled() => return,
                result = client.send_raw_request(ticket.operation.method, params) => result,
            }
            .map_err(|error| error.to_string())
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
        if let Err(error) = &result {
            tracing::error!(session_id = %ticket.session.id, method = ticket.operation.method, %error, "Plugin operation failed");
        }
        loop {
            if ticket.cancelled.is_cancelled() || SearchSession::current() != ticket.session {
                return;
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
    if let Some(operation) = PluginOperation::installed(action, plugin) {
        if action == "uninstall" {
            state.write().confirmation = Some(operation);
            *confirm_action.write() = Some("uninstall".into());
        } else {
            dispatch(operation, state);
            *detail_plugin_idx.write() = None;
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

#[cfg(test)]
#[path = "operation_test.rs"]
mod tests;
