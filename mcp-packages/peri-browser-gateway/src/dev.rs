//! Standalone development session discovered from Kitesurf.
//!
//! With `--dev` and no MCP, nobody registers over the Unix socket. The gateway
//! therefore publishes one synthetic session whose pages come from the
//! Kitesurf HTTP endpoint `GET /json/list` and refreshes it on a short timer.
//!
//! This is the only place the gateway reads `/json/list`: normal operation is
//! registration-driven and never guesses page ownership (README §6.1). No CDP
//! connection is opened here; the next stage attaches viewers to these pages.

use std::sync::Arc;
use std::time::Duration;

use reqwest::Client;
use serde::Deserialize;
use tokio::time::sleep;
use tracing::{debug, info, warn};

use crate::protocol::{AgentState, PageRef};
use crate::session::SessionStore;

/// Synthetic session used in development mode.
pub const DEV_SESSION_ID: &str = "dev";
pub const DEV_SESSION_NAME: &str = "dev";
/// Discovery cadence. Kitesurf pages change at human speed; two seconds keeps
/// the tab strip responsive without polling the engine in a tight loop.
pub const DEV_POLL_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Debug, thiserror::Error)]
enum DiscoveryError {
    #[error("GET {url} failed: {source}")]
    Request {
        url: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("GET {url} returned HTTP {status}")]
    Status {
        url: String,
        status: reqwest::StatusCode,
    },
    #[error("GET {url} returned an invalid target list: {source}")]
    Decode {
        url: String,
        #[source]
        source: serde_json::Error,
    },
}

#[derive(Debug, Deserialize)]
struct DevToolsTarget {
    #[serde(default)]
    id: String,
    #[serde(rename = "type", default)]
    kind: Option<String>,
    #[serde(default)]
    url: String,
    #[serde(default)]
    title: String,
}

/// Registers the synthetic session and starts refreshing its page list.
///
/// The session exists even before the first successful discovery, so `/` shows
/// a monitor URL immediately and an unreachable engine is visible as an empty
/// page list with a warning in the log.
pub async fn start(
    store: Arc<SessionStore>,
    cdp_url: String,
    http_base: String,
    monitor_base: String,
) {
    store
        .register(
            DEV_SESSION_ID,
            DEV_SESSION_NAME,
            cdp_url.clone(),
            Vec::new(),
            AgentState::Idle,
        )
        .await;
    info!(
        monitor = %monitor_base,
        "dev session registered; discovering pages from {http_base}/json/list"
    );

    tokio::spawn(poll(store, cdp_url, http_base, monitor_base));
}

async fn poll(store: Arc<SessionStore>, cdp_url: String, http_base: String, monitor_base: String) {
    let list_url = format!("{http_base}/json/list");
    let client = match Client::builder().timeout(Duration::from_secs(3)).build() {
        Ok(client) => client,
        Err(error) => {
            warn!(error = %error, "cannot build the development HTTP client");
            return;
        }
    };

    let mut last: Option<Vec<PageRef>> = None;
    let mut failing = false;
    loop {
        match fetch_pages(&client, &list_url).await {
            Ok(pages) => {
                if failing {
                    info!(url = %list_url, "dev discovery recovered");
                    failing = false;
                }
                if last.as_ref() != Some(&pages) {
                    let count = pages.len();
                    if store
                        .update_pages(DEV_SESSION_ID, pages.clone())
                        .await
                        .is_none()
                    {
                        // The synthetic session was unregistered while the
                        // gateway was running; recreate it with the same id so
                        // the monitor URL stays valid.
                        store
                            .register(
                                DEV_SESSION_ID,
                                DEV_SESSION_NAME,
                                cdp_url.clone(),
                                pages.clone(),
                                AgentState::Idle,
                            )
                            .await;
                        info!(
                            monitor = %monitor_base,
                            "dev session re-registered"
                        );
                    }
                    debug!(pages = count, "dev page list updated");
                    last = Some(pages);
                }
            }
            Err(error) => {
                if failing {
                    debug!(error = %error, "dev discovery still failing");
                } else {
                    warn!(error = %error, "dev discovery failed");
                    failing = true;
                }
            }
        }
        sleep(DEV_POLL_INTERVAL).await;
    }
}

async fn fetch_pages(client: &Client, url: &str) -> Result<Vec<PageRef>, DiscoveryError> {
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|source| DiscoveryError::Request {
            url: url.to_owned(),
            source,
        })?;
    let status = response.status();
    if !status.is_success() {
        return Err(DiscoveryError::Status {
            url: url.to_owned(),
            status,
        });
    }
    let body = response
        .text()
        .await
        .map_err(|source| DiscoveryError::Request {
            url: url.to_owned(),
            source,
        })?;
    parse_targets(&body).map_err(|source| DiscoveryError::Decode {
        url: url.to_owned(),
        source,
    })
}

/// Converts a DevTools target list into the gateway's page references.
///
/// Non-page targets (when Kitesurf exposes any) and entries without a target id
/// are skipped. Order is preserved so the first page remains the default tab.
fn parse_targets(body: &str) -> Result<Vec<PageRef>, serde_json::Error> {
    let targets: Vec<DevToolsTarget> = serde_json::from_str(body)?;
    Ok(targets
        .into_iter()
        .filter(|target| !target.id.is_empty())
        .filter(|target| target.kind.as_deref().is_none_or(|kind| kind == "page"))
        .map(|target| PageRef {
            target_id: target.id,
            url: target.url,
            title: target.title,
        })
        .collect())
}

#[cfg(test)]
#[path = "dev_test.rs"]
mod tests;
