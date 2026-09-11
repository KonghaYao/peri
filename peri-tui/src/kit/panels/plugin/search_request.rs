//! Request responses are owned by a search ticket, independently of legacy notifications.

use super::{
    PluginSearchResultItem,
    discover::{SearchSession, SearchTicket},
};
use crate::acp_client::AcpTuiClient;
use std::{sync::Arc, time::Duration};

pub(super) type SearchResult = Result<Vec<PluginSearchResultItem>, String>;

pub(super) fn launch_search(
    ticket: SearchTicket,
    client: Option<Arc<AcpTuiClient>>,
    // Return an unconsumed result when the hook is temporarily borrowed.
    complete: impl Fn(&SearchTicket, SearchResult) -> Option<SearchResult> + Send + 'static,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut result = if let Some(client) = client {
            let params = serde_json::json!({"query": ticket.query, "sessionId": ticket.session.id});
            tokio::select! {
                biased;
                _ = ticket.cancelled.cancelled() => return,
                result = client.send_raw_request("plugin/search", params) => result,
            }
            .map_err(|error| error.to_string())
            .and_then(decode_results)
        } else {
            Err("ACP client not available".into())
        };
        loop {
            if ticket.cancelled.is_cancelled() || SearchSession::current() != ticket.session {
                return;
            }
            let Some(unconsumed) = complete(&ticket, result) else {
                return;
            };
            result = unconsumed;
            // A render read guard may outlive RPC completion. Keep the result
            // until accepted without spinning or retaining the hook owner;
            // owner Drop, edit, and reset cancel the ticket and this wait.
            tokio::select! {
                biased;
                _ = ticket.cancelled.cancelled() => return,
                _ = tokio::time::sleep(Duration::from_millis(1)) => {}
            }
        }
    })
}

fn decode_results(value: serde_json::Value) -> Result<Vec<PluginSearchResultItem>, String> {
    let results = value
        .get("results")
        .cloned()
        .ok_or_else(|| crate::i18n::tr("panel-plugin-search-invalid-response"))?;
    serde_json::from_value(results)
        .map_err(|_| crate::i18n::tr("panel-plugin-search-invalid-response"))
}
