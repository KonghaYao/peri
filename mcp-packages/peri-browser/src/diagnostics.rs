//! Per-page console and network diagnostics.
//!
//! Events arrive on the page controller. `Runtime` and `Network` are enabled
//! lazily (when the corresponding tool is first used), and each page keeps a
//! bounded newest-wins buffer so a chatty page cannot grow the process.

use serde::Serialize;
use serde_json::{Map, Value};

use crate::ring::RingBuffer;

/// Entries retained per page and diagnostic kind (README §7).
pub const DIAGNOSTIC_CAPACITY: usize = 200;
/// Largest response body returned by `get_network_request`.
pub const MAX_BODY_BYTES: usize = 64 * 1024;

/// One console line or uncaught exception.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsoleEntry {
    pub level: String,
    pub text: String,
    pub timestamp_ms: f64,
}

/// One network request as buffered from `Network.*`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkEntry {
    pub request_id: String,
    pub url: String,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub started_ms: f64,
    pub finished: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_headers: Option<Map<String, Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_headers: Option<Map<String, Value>>,
}

/// Tracks which domains are enabled and owns both ring buffers.
#[derive(Debug, Clone)]
pub struct Diagnostics {
    runtime_enabled: bool,
    network_enabled: bool,
    console: RingBuffer<ConsoleEntry>,
    network: RingBuffer<NetworkEntry>,
}

impl Default for Diagnostics {
    fn default() -> Self {
        Self::new()
    }
}

impl Diagnostics {
    #[must_use]
    pub fn new() -> Self {
        Self {
            runtime_enabled: false,
            network_enabled: false,
            console: RingBuffer::new(DIAGNOSTIC_CAPACITY),
            network: RingBuffer::new(DIAGNOSTIC_CAPACITY),
        }
    }

    #[must_use]
    pub fn runtime_enabled(&self) -> bool {
        self.runtime_enabled
    }

    pub fn mark_runtime_enabled(&mut self) {
        self.runtime_enabled = true;
    }

    #[must_use]
    pub fn network_enabled(&self) -> bool {
        self.network_enabled
    }

    pub fn mark_network_enabled(&mut self) {
        self.network_enabled = true;
    }

    #[must_use]
    pub fn console(&self) -> &RingBuffer<ConsoleEntry> {
        &self.console
    }

    #[must_use]
    pub fn network(&self) -> &RingBuffer<NetworkEntry> {
        &self.network
    }

    pub fn clear(&mut self) {
        self.console.clear();
        self.network.clear();
    }

    /// Records one CDP event. Unknown methods are ignored.
    pub fn observe(&mut self, method: &str, params: &Value) {
        match method {
            "Runtime.consoleAPICalled" => self.observe_console(params),
            "Runtime.exceptionThrown" => self.observe_exception(params),
            "Network.requestWillBeSent" => self.observe_request(params),
            "Network.responseReceived" => self.observe_response(params),
            "Network.loadingFinished" => self.observe_finished(params, None),
            "Network.loadingFailed" => self.observe_loading_failed(params),
            _ => {}
        }
    }

    fn observe_console(&mut self, params: &Value) {
        let level = params
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("log")
            .to_owned();
        let text = params
            .get("args")
            .and_then(Value::as_array)
            .map(|args| args.iter().map(remote_text).collect::<Vec<_>>().join(" "))
            .unwrap_or_default();
        let timestamp_ms = params
            .get("timestamp")
            .and_then(Value::as_f64)
            .unwrap_or_default();
        self.console.push(ConsoleEntry {
            level,
            text,
            timestamp_ms,
        });
    }

    fn observe_exception(&mut self, params: &Value) {
        let details = params.get("exceptionDetails").unwrap_or(&Value::Null);
        let mut text = details
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or("uncaught exception")
            .to_owned();
        if let Some(description) = details
            .get("exception")
            .and_then(|exception| exception.get("description"))
            .and_then(Value::as_str)
        {
            if !description.is_empty() {
                text.push_str(": ");
                text.push_str(description);
            }
        }
        let timestamp_ms = details
            .get("timestamp")
            .and_then(Value::as_f64)
            .unwrap_or_default();
        self.console.push(ConsoleEntry {
            level: "error".to_owned(),
            text,
            timestamp_ms,
        });
    }

    fn observe_request(&mut self, params: &Value) {
        let Some(request_id) = params.get("requestId").and_then(Value::as_str) else {
            return;
        };
        let request = params.get("request").unwrap_or(&Value::Null);
        let url = request
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let method = request
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("GET")
            .to_owned();
        let request_headers = request.get("headers").and_then(Value::as_object).cloned();
        let started_ms = params
            .get("timestamp")
            .and_then(Value::as_f64)
            .unwrap_or_default();
        let resource_type = params
            .get("type")
            .and_then(Value::as_str)
            .map(str::to_owned);
        self.network.push(NetworkEntry {
            request_id: request_id.to_owned(),
            url,
            method,
            status: None,
            status_text: None,
            mime_type: None,
            resource_type,
            error: None,
            started_ms,
            finished: false,
            request_headers,
            response_headers: None,
        });
    }

    fn observe_response(&mut self, params: &Value) {
        let Some(request_id) = params.get("requestId").and_then(Value::as_str) else {
            return;
        };
        let response = params.get("response").unwrap_or(&Value::Null);
        let status = response
            .get("status")
            .and_then(Value::as_u64)
            .and_then(|status| u16::try_from(status).ok());
        let status_text = response
            .get("statusText")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let mime_type = response
            .get("mimeType")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let response_headers = response.get("headers").and_then(Value::as_object).cloned();
        if let Some(entry) = find_network(&mut self.network, request_id) {
            entry.status = status;
            entry.status_text = status_text;
            entry.mime_type = mime_type;
            entry.response_headers = response_headers;
        }
    }

    fn observe_finished(&mut self, params: &Value, error: Option<String>) {
        let Some(request_id) = params.get("requestId").and_then(Value::as_str) else {
            return;
        };
        if let Some(entry) = find_network(&mut self.network, request_id) {
            entry.finished = true;
            if error.is_some() {
                entry.error = error;
            }
        }
    }

    fn observe_loading_failed(&mut self, params: &Value) {
        let error = params
            .get("errorText")
            .and_then(Value::as_str)
            .unwrap_or("request failed")
            .to_owned();
        self.observe_finished(params, Some(error));
    }

    /// Newest-first network summaries, optionally filtered by a substring of
    /// the URL or by HTTP method.
    #[must_use]
    pub fn list_network(&self, filter: Option<&str>) -> Vec<NetworkEntry> {
        let needle = filter.map(str::to_ascii_lowercase);
        self.network
            .iter()
            .rev()
            .filter(|entry| match &needle {
                Some(needle) => {
                    entry.url.to_ascii_lowercase().contains(needle)
                        || entry.method.to_ascii_lowercase() == *needle
                }
                None => true,
            })
            .cloned()
            .collect()
    }

    #[must_use]
    pub fn get_network(&self, request_id: &str) -> Option<NetworkEntry> {
        self.network
            .iter()
            .rev()
            .find(|entry| entry.request_id == request_id)
            .cloned()
    }

    #[must_use]
    pub fn console_entries(&self) -> Vec<ConsoleEntry> {
        self.console.iter().cloned().collect()
    }
}

fn find_network<'a>(
    buffer: &'a mut RingBuffer<NetworkEntry>,
    request_id: &str,
) -> Option<&'a mut NetworkEntry> {
    buffer
        .iter_mut()
        .rev()
        .find(|entry| entry.request_id == request_id)
}

fn remote_text(value: &Value) -> String {
    if let Some(text) = value.get("value").and_then(Value::as_str) {
        return text.to_owned();
    }
    if let Some(value) = value.get("value").filter(|value| !value.is_null()) {
        return value.to_string();
    }
    if let Some(description) = value.get("description").and_then(Value::as_str) {
        return description.to_owned();
    }
    if let Some(unserializable) = value.get("unserializableValue").and_then(Value::as_str) {
        return unserializable.to_owned();
    }
    value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("undefined")
        .to_owned()
}

#[cfg(test)]
#[path = "diagnostics_test.rs"]
mod tests;
