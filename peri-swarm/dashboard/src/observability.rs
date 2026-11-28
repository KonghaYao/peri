//! Minimal async client for the OpenObserve query API.
//!
//! The dashboard connects directly with a dedicated OpenObserve account. OSS
//! v1.2.0 has no viewer/editor role, so that account is an admin and can write.
//! The client only sends `SELECT` statements, but that is an operator-safety
//! check, **not** a security boundary: protection depends on the private
//! network, the dedicated account, and the `0600` credential file.
//!
//! Credentials never appear in `Debug`, error messages, or logs. Errors carry
//! only a status code, its canonical reason, and a scrubbed short server
//! message.

use std::fmt;

use reqwest::redirect::Policy;
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::Url;

use crate::config::{ObservabilityConfig, ObservabilityConfigError};

/// Query/test timeout. The gateway client uses the same budget.
pub const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
/// Longest server message retained in an error.
const MAX_ERROR_DETAIL: usize = 160;
const REDACTED: &str = "<redacted>";

/// Failure from an OpenObserve request.
///
/// Every variant is safe to display: no credentials, headers, SQL text, or
/// response bodies are retained.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ObservabilityError {
    /// The connection values failed validation.
    #[error("observability settings are invalid: {0}")]
    InvalidConfig(#[from] ObservabilityConfigError),
    /// The request exceeded [`REQUEST_TIMEOUT`].
    #[error("OpenObserve request timed out")]
    Timeout,
    /// The host could not be reached.
    #[error("OpenObserve is unreachable")]
    Unreachable,
    /// A non-2xx response. `detail` is a canonical reason plus an optional
    /// scrubbed server message.
    #[error("OpenObserve returned HTTP {status}{detail}")]
    Http {
        /// HTTP status code.
        status: u16,
        /// `" Unauthorized"` or `": short message"`, already scrubbed.
        detail: String,
    },
    /// The response was not the documented JSON shape.
    #[error("OpenObserve returned an unreadable response")]
    Malformed,
    /// The HTTP client could not be built.
    #[error("OpenObserve client could not be built")]
    Client,
}

/// Result alias for this module.
pub type Result<T> = std::result::Result<T, ObservabilityError>;

/// One parsed `_search` response.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SearchPage {
    /// Result rows, in server order.
    pub hits: Vec<Value>,
    /// Total rows matching the query, as reported by OpenObserve.
    pub total: u64,
}

/// OpenObserve search response (the fields the dashboard needs).
#[derive(Deserialize)]
struct SearchResponse {
    #[serde(default)]
    hits: Vec<Value>,
    #[serde(default)]
    total: u64,
}

/// OpenObserve streams response (used only as a lightweight probe).
#[derive(Deserialize)]
struct StreamsResponse {
    #[serde(default)]
    list: Vec<Value>,
}

#[derive(Serialize)]
struct SearchRequest<'a> {
    query: SearchQuery<'a>,
}

#[derive(Serialize)]
struct SearchQuery<'a> {
    sql: &'a str,
    start_time: u64,
    end_time: u64,
    from: u64,
    size: u64,
}

/// Basic-auth client for one OpenObserve organization.
pub struct OpenObserveClient {
    base_url: String,
    org: String,
    user: String,
    password: String,
    client: Client,
}

impl fmt::Debug for OpenObserveClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpenObserveClient")
            .field("base_url", &self.base_url)
            .field("org", &self.org)
            .field("password", &REDACTED)
            .finish_non_exhaustive()
    }
}

impl OpenObserveClient {
    /// Validate candidate values and build a client.
    pub fn new(base_url: &str, org: &str, user: &str, password: &str) -> Result<Self> {
        let config = ObservabilityConfig::new(base_url, org, user, password)?;
        Self::from_config(&config)
    }

    /// Build a client from validated values.
    pub fn from_config(config: &ObservabilityConfig) -> Result<Self> {
        let client = Client::builder()
            .timeout(REQUEST_TIMEOUT)
            // Credentials must never be forwarded to a redirect target.
            .redirect(Policy::none())
            .build()
            .map_err(|_| ObservabilityError::Client)?;
        Ok(Self {
            base_url: config.base_url().to_owned(),
            org: config.org().to_owned(),
            user: config.user().to_owned(),
            password: config.password().to_owned(),
            client,
        })
    }

    /// Run one SQL query over `[start_us, end_us)` microseconds.
    ///
    /// `from` and `size` paginate the result set. The server response must
    /// carry `hits` and `total`; anything else is reported as malformed.
    pub async fn search(
        &self,
        sql: &str,
        start_us: u64,
        end_us: u64,
        from: u64,
        size: u64,
    ) -> Result<SearchPage> {
        let url = self.endpoint(&["api", &self.org, "_search"])?;
        let request = SearchRequest {
            query: SearchQuery {
                sql,
                start_time: start_us,
                end_time: end_us,
                from,
                size,
            },
        };
        let response = self
            .client
            .post(url)
            .basic_auth(&self.user, Some(&self.password))
            .json(&request)
            .send()
            .await
            .map_err(transport_error)?;
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|_| ObservabilityError::Malformed)?;
        if !status.is_success() {
            return Err(self.http_error(status, &body));
        }
        let parsed: SearchResponse =
            serde_json::from_str(&body).map_err(|_| ObservabilityError::Malformed)?;
        Ok(SearchPage {
            hits: parsed.hits,
            total: parsed.total,
        })
    }

    /// Probe the organization with `GET /api/{org}/streams`.
    ///
    /// Returns the stream count when the response exposes it, otherwise
    /// `None`; a 2xx response is a successful connection either way.
    pub async fn test_connection(&self) -> Result<Option<usize>> {
        let url = self.endpoint(&["api", &self.org, "streams"])?;
        let response = self
            .client
            .get(url)
            .basic_auth(&self.user, Some(&self.password))
            .send()
            .await
            .map_err(transport_error)?;
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|_| ObservabilityError::Malformed)?;
        if !status.is_success() {
            return Err(self.http_error(status, &body));
        }
        Ok(serde_json::from_str::<StreamsResponse>(&body)
            .ok()
            .map(|streams| streams.list.len()))
    }

    /// Build `{base_url}/api/{org}/{...}` with percent-encoded segments.
    fn endpoint(&self, segments: &[&str]) -> Result<Url> {
        let mut url =
            Url::parse(&format!("{}/", self.base_url)).map_err(|_| ObservabilityError::Client)?;
        let mut path = url
            .path_segments_mut()
            .map_err(|_| ObservabilityError::Client)?;
        for segment in segments {
            path.push(segment);
        }
        drop(path);
        Ok(url)
    }

    /// Safe error for a non-2xx response.
    fn http_error(&self, status: StatusCode, body: &str) -> ObservabilityError {
        let mut detail = String::new();
        if let Some(reason) = status.canonical_reason() {
            detail.push(' ');
            detail.push_str(reason);
        }
        if let Some(message) = short_server_message(body) {
            let message = self.scrub(&message);
            if !message.is_empty() {
                detail.push_str(": ");
                detail.push_str(&message);
            }
        }
        ObservabilityError::Http {
            status: status.as_u16(),
            detail,
        }
    }

    /// Remove the password from a server message and bound its length.
    fn scrub(&self, message: &str) -> String {
        let mut scrubbed = message.replace(&self.password, REDACTED);
        scrubbed = scrubbed
            .chars()
            .map(|character| {
                if character.is_control() {
                    ' '
                } else {
                    character
                }
            })
            .take(MAX_ERROR_DETAIL)
            .collect();
        scrubbed.trim().to_owned()
    }
}

/// Extract a short, human-readable message from an error body.
///
/// Only a string field is retained; the raw body is never propagated.
fn short_server_message(body: &str) -> Option<String> {
    let value: Value = serde_json::from_str(body).ok()?;
    for candidate in [
        value.get("message"),
        value.get("error").and_then(|error| error.get("message")),
        value.get("error"),
    ] {
        if let Some(text) = candidate.and_then(Value::as_str) {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_owned());
            }
        }
    }
    None
}

/// Map a transport failure to a safe, credential-free variant.
fn transport_error(error: reqwest::Error) -> ObservabilityError {
    if error.is_timeout() {
        ObservabilityError::Timeout
    } else {
        ObservabilityError::Unreachable
    }
}

#[cfg(test)]
#[path = "observability_test.rs"]
mod tests;
