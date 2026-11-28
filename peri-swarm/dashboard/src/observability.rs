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

use std::collections::BTreeMap;
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

/// One PromQL sample. Timestamps are Unix seconds, as returned by OpenObserve.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PromSample {
    /// Evaluation timestamp in Unix seconds.
    pub timestamp: i64,
    /// Sample value. Non-finite values are filtered out by consumers.
    pub value: f64,
}

/// One PromQL time series.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PromSeries {
    /// Series labels; `__name__` is included when the server reports it.
    pub labels: BTreeMap<String, String>,
    /// Samples in ascending timestamp order.
    pub values: Vec<PromSample>,
}

/// Parsed PromQL `query_range` result.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PromMatrix {
    /// Matching series.
    pub series: Vec<PromSeries>,
}

/// PromQL query response envelope.
#[derive(Deserialize)]
struct PromEnvelope {
    status: String,
    #[serde(default)]
    data: Option<PromData>,
}

/// PromQL result payload.
#[derive(Deserialize)]
struct PromData {
    #[serde(rename = "resultType")]
    result_type: String,
    #[serde(default)]
    result: Vec<PromSeriesRaw>,
}

/// One raw PromQL series.
#[derive(Deserialize)]
struct PromSeriesRaw {
    #[serde(default)]
    metric: BTreeMap<String, String>,
    #[serde(default)]
    values: Vec<(f64, Value)>,
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

    /// Run one PromQL range query over `[start_s, end_s]` seconds.
    ///
    /// OpenObserve stores OTLP metrics in Prometheus-compatible streams and
    /// rejects SQL search on them, so this is the only metrics query path.
    /// `step_s` is the evaluation step in seconds.
    pub async fn query_range(
        &self,
        query: &str,
        start_s: u64,
        end_s: u64,
        step_s: u64,
    ) -> Result<PromMatrix> {
        let mut url =
            self.endpoint(&["api", &self.org, "prometheus", "api", "v1", "query_range"])?;
        {
            let mut params = url.query_pairs_mut();
            params.append_pair("query", query);
            params.append_pair("start", &start_s.to_string());
            params.append_pair("end", &end_s.to_string());
            params.append_pair("step", &step_s.to_string());
        }
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
        let envelope: PromEnvelope =
            serde_json::from_str(&body).map_err(|_| ObservabilityError::Malformed)?;
        if envelope.status != "success" {
            return Err(ObservabilityError::Malformed);
        }
        let data = envelope.data.ok_or(ObservabilityError::Malformed)?;
        if data.result_type != "matrix" {
            return Err(ObservabilityError::Malformed);
        }
        let series = data
            .result
            .into_iter()
            .map(|raw| PromSeries {
                labels: raw.metric,
                values: raw
                    .values
                    .into_iter()
                    .filter_map(|(timestamp, value)| {
                        parse_prom_value(&value).map(|value| PromSample {
                            timestamp: timestamp as i64,
                            value,
                        })
                    })
                    .collect(),
            })
            .collect();
        Ok(PromMatrix { series })
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
        let scrubbed = message.replace(&self.password, REDACTED);
        let mut scrubbed = redact_authorization_value(&scrubbed);
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

/// Redact the value after an `Authorization:` header name.
///
/// A server error body may echo request headers. The header name is harmless;
/// its value is not, so replace the scheme and credential tokens that follow
/// the header name.
fn redact_authorization_value(message: &str) -> String {
    const KEY: &str = "authorization";
    let mut result = String::with_capacity(message.len());
    let mut rest = message;
    loop {
        let lower = rest.to_ascii_lowercase();
        let Some(position) = lower.find(KEY) else {
            result.push_str(rest);
            break;
        };
        let header_end = position + KEY.len();
        result.push_str(&rest[..header_end]);
        let after = &rest[header_end..];
        let Some(colon) = after.find(':') else {
            rest = after;
            continue;
        };
        let value_start = header_end + colon + 1;
        result.push_str(&rest[header_end..value_start]);
        let value = &rest[value_start..];
        let leading = value.len() - value.trim_start().len();
        result.push_str(&value[..leading]);
        let token = &value[leading..];
        let first_end = token_end(token);
        if first_end > 0 {
            // `Authorization: Basic <credential>` and `Bearer <token>` carry
            // the secret in the second token; redact both.
            let scheme = token[..first_end].to_ascii_lowercase();
            let extra = if matches!(scheme.as_str(), "basic" | "bearer" | "digest" | "negotiate") {
                let after_scheme = &token[first_end..];
                let spaces = after_scheme.len() - after_scheme.trim_start().len();
                spaces + token_end(&after_scheme[spaces..])
            } else {
                0
            };
            result.push_str(REDACTED);
            rest = &token[(first_end + extra).min(token.len())..];
        } else {
            rest = token;
        }
    }
    result
}

/// Index of the first whitespace/quote/comma delimiter, or the token length.
fn token_end(token: &str) -> usize {
    token
        .find(|character: char| character.is_whitespace() || character == '"' || character == ',')
        .unwrap_or(token.len())
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

/// Parse a PromQL sample value. The Prometheus JSON format encodes it as a
/// string, but local OpenObserve builds have returned bare numbers too.
fn parse_prom_value(value: &Value) -> Option<f64> {
    if let Some(number) = value.as_f64() {
        return Some(number);
    }
    let text = value.as_str()?;
    match text {
        "NaN" => Some(f64::NAN),
        "+Inf" | "Inf" => Some(f64::INFINITY),
        "-Inf" => Some(f64::NEG_INFINITY),
        _ => text.parse().ok(),
    }
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
