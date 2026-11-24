use super::{CdpError, PageController};
use serde_json::{json, Value};
use std::time::Duration;

const NAVIGATION_SETTLE_TRIES: usize = 20;
const NAVIGATION_SETTLE_DELAY: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NavigationState {
    pub url: String,
    pub title: String,
    pub can_go_back: bool,
    pub can_go_forward: bool,
}

#[derive(Debug, Clone)]
struct HistoryEntry {
    url: String,
    title: String,
}

#[derive(Debug, Clone)]
struct History {
    current: usize,
    entries: Vec<HistoryEntry>,
}

fn parse_history(value: &Value) -> Option<History> {
    let current = usize::try_from(value.get("currentIndex")?.as_i64()?).ok()?;
    let entries = value
        .get("entries")?
        .as_array()?
        .iter()
        .map(|entry| {
            Some(HistoryEntry {
                url: entry.get("url")?.as_str()?.to_owned(),
                title: entry
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            })
        })
        .collect::<Option<Vec<_>>>()?;
    if entries.is_empty() || current >= entries.len() {
        return None;
    }
    Some(History { current, entries })
}

async fn read_history(controller: &dyn PageController) -> Result<History, CdpError> {
    let value = controller
        .command("Page.getNavigationHistory", json!({}))
        .await?;
    parse_history(&value).ok_or_else(|| CdpError::Decode("no navigation entries".to_owned()))
}

async fn document_title(controller: &dyn PageController) -> Option<String> {
    let value = controller
        .command(
            "Runtime.evaluate",
            json!({"expression": "document.title", "returnByValue": true}),
        )
        .await
        .ok()?;
    value
        .get("result")?
        .get("value")?
        .as_str()
        .map(str::to_owned)
}

fn navigation_state_from(history: &History, title: Option<String>) -> NavigationState {
    let entry = &history.entries[history.current];
    NavigationState {
        url: entry.url.clone(),
        title: title.filter(|title| !title.is_empty()).unwrap_or_else(|| {
            if entry.title.is_empty() {
                entry.url.clone()
            } else {
                entry.title.clone()
            }
        }),
        can_go_back: history.current > 0,
        can_go_forward: history.current + 1 < history.entries.len(),
    }
}

/// Reads the controller's current navigation state.
///
/// The live `location.href` wins over the history entry: Kitesurf keeps
/// `Page.getNavigationHistory` on the previous document for a few seconds after
/// a navigation commits, and the address bar must not lag behind the page.
///
/// # Errors
///
/// Fails when the history cannot be read or parsed.
pub async fn navigation_state(
    controller: &dyn PageController,
) -> Result<NavigationState, CdpError> {
    let history = read_history(controller).await?;
    let title = document_title(controller).await;
    let live = live_url(controller).await;
    Ok(state_with_live_location(&history, title, live.as_deref()))
}

fn state_with_live_location(
    history: &History,
    title: Option<String>,
    live: Option<&str>,
) -> NavigationState {
    let mut state = navigation_state_from(history, title);
    if let Some(live) = live.filter(|live| *live != history.entries[history.current].url) {
        // The history has not caught up with the document that just committed;
        // the new entry will land one step forward.
        state.url = live.to_owned();
        state.can_go_back = true;
        state.can_go_forward = false;
    }
    state
}

async fn live_url(controller: &dyn PageController) -> Option<String> {
    let value = controller
        .command(
            "Runtime.evaluate",
            json!({"expression": "location.href", "returnByValue": true}),
        )
        .await
        .ok()?;
    value
        .get("result")?
        .get("value")?
        .as_str()
        .map(str::to_owned)
}

/// Navigates the controller's page and reports the resulting state.
///
/// Kitesurf never answers `Page.navigate` and stops answering every later
/// command on that connection, so the navigation is written as
/// `location.assign(...)` through `Runtime.evaluate`, which it does answer.
/// It sometimes answers that evaluate with `Execution context was destroyed`
/// once the new document took over; the effect decides, not the reply.
///
/// # Errors
///
/// Fails when the page rejects the evaluation and stayed where it was, or when
/// the follow-up history read fails.
pub async fn navigate(
    controller: &dyn PageController,
    url: &str,
) -> Result<NavigationState, CdpError> {
    let url = normalize_url(url);
    let before = live_url(controller).await;
    let expression = format!("location.assign({})", Value::String(url));
    let mut retried = false;
    loop {
        let reply = controller
            .command(
                "Runtime.evaluate",
                json!({"expression": expression, "userGesture": true}),
            )
            .await;
        let changed = wait_for_location(controller, before.as_deref()).await;
        if let Ok(value) = &reply {
            if let Some(exception) = value.get("exceptionDetails") {
                return Err(CdpError::Rejected {
                    method: "Runtime.evaluate".to_owned(),
                    message: exception_message(exception),
                });
            }
        }
        if let Err(error) = reply {
            if changed {
                tracing::debug!("CDP 导航已提交，忽略求值错误");
                return navigation_state(controller).await;
            }
            // Kitesurf answers `Execution context was destroyed` for a moment
            // after every navigation, and then the assignment never ran. One
            // retry separates that from a navigation that really committed.
            if !retried && destructured_retry_allowed(&error) {
                retried = true;
                tokio::time::sleep(NAVIGATION_SETTLE_DELAY).await;
                continue;
            }
            return Err(error);
        }
        return navigation_state(controller).await;
    }
}

/// Whether the error is the stale-context answer Kitesurf gives while the
/// previous document is still being torn down.
fn destructured_retry_allowed(error: &CdpError) -> bool {
    matches!(error, CdpError::Protocol { code: -32000, message } if message.contains("Execution context was destroyed"))
}

/// Gives a bare host the scheme a browser would pick.
///
/// `example.com` typed into the address bar must not resolve relative to the
/// current document; `Page.navigate` would have done exactly that.
pub(super) fn normalize_url(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return trimmed.to_owned();
    }
    let has_scheme = trimmed.split_once(':').is_some_and(|(scheme, rest)| {
        !scheme.is_empty()
            && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
            && scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
            && (rest.starts_with("//")
                || matches!(
                    scheme,
                    "about" | "blob" | "data" | "file" | "javascript" | "view-source"
                ))
    });
    if has_scheme {
        return trimmed.to_owned();
    }
    // Chrome falls back to http for the loopback names people type.
    if ["localhost", "127.", "[::1]"]
        .iter()
        .any(|prefix| trimmed.starts_with(prefix))
    {
        return format!("http://{trimmed}");
    }
    format!("https://{trimmed}")
}

fn exception_message(exception: &Value) -> String {
    exception
        .get("exception")
        .and_then(|exception| exception.get("description"))
        .and_then(Value::as_str)
        .or_else(|| exception.get("text").and_then(Value::as_str))
        .unwrap_or("the page rejected the URL")
        .to_owned()
}

/// Waits briefly for the document to commit so the caller reports the new URL.
///
/// Kitesurf's evaluate reply can arrive before the commit; the history read is
/// the slow one, so the live location is polled instead of sleeping a fixed
/// amount. Returns whether the location moved away from `before`.
async fn wait_for_location(controller: &dyn PageController, before: Option<&str>) -> bool {
    for attempt in 0..NAVIGATION_SETTLE_TRIES {
        if live_url(controller)
            .await
            .as_deref()
            .is_some_and(|live| Some(live) != before)
        {
            return true;
        }
        if attempt + 1 < NAVIGATION_SETTLE_TRIES {
            tokio::time::sleep(NAVIGATION_SETTLE_DELAY).await;
        }
    }
    false
}
