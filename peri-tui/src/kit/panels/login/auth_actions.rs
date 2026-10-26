//! Login OS actions, transient notifications, and test dispatch seams.
//!
//! Kept out of `auth.rs` so the controller state machine stays within the file
//! budget. These helpers hold no credential state: the URL is passed straight to
//! the OS/clipboard adapter and is never logged or persisted here.

use peri_acp::provider::auth::LogoutOutcome;

use crate::i18n;
use crate::kit::atoms::{NOTIFICATION, Notification};

use super::runtime_handle;

#[cfg(test)]
thread_local! {
    static TEST_DISPATCHES: std::cell::RefCell<Vec<&'static str>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

#[cfg(test)]
pub(crate) fn take_test_dispatches() -> Vec<&'static str> {
    TEST_DISPATCHES.with(|slot| std::mem::take(&mut *slot.borrow_mut()))
}

#[cfg(test)]
pub(super) fn intercept_dispatch(kind: &'static str) -> bool {
    TEST_DISPATCHES.with(|slot| slot.borrow_mut().push(kind));
    true
}

// ── Separate OS actions ───────────────────────────────────────────────────────

#[cfg(test)]
thread_local! {
    static TEST_URL_ACTIONS: std::cell::RefCell<Vec<(&'static str, String)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

#[cfg(test)]
pub(crate) fn take_test_url_actions() -> Vec<(&'static str, String)> {
    TEST_URL_ACTIONS.with(|slot| std::mem::take(&mut *slot.borrow_mut()))
}

#[cfg(test)]
fn intercept_url_action(kind: &'static str, url: &str) -> bool {
    TEST_URL_ACTIONS.with(|slot| slot.borrow_mut().push((kind, url.to_owned())));
    true
}

/// Request the OS browser. Never claims authorization succeeded.
pub(crate) fn open_browser(url: String) {
    #[cfg(test)]
    if intercept_url_action("open", &url) {
        return;
    }
    let Some(handle) = runtime_handle() else {
        notify_key("login-auth-browser-failed", 3);
        return;
    };
    handle.spawn_blocking(move || match crate::kit::auth_browser::open_browser(&url) {
        Ok(()) => notify_key("login-auth-browser-requested", 2),
        Err(error) => {
            tracing::warn!(%error, "failed to launch browser for ChatGPT sign-in");
            notify_key("login-auth-browser-failed", 3);
        }
    });
}

/// Copy the authorization URL through the existing clipboard channel.
pub(crate) fn copy_url(url: String) {
    #[cfg(test)]
    if intercept_url_action("copy", &url) {
        return;
    }
    let Some(handle) = runtime_handle() else {
        notify_key("login-auth-copy-failed", 3);
        return;
    };
    handle.spawn_blocking(move || match crate::kit::clipboard::copy_text(&url) {
        Ok(()) => notify_key("login-auth-url-copied", 2),
        Err(error) => {
            tracing::warn!(%error, "failed to copy ChatGPT authorization URL");
            notify_key("login-auth-copy-failed", 3);
        }
    });
}

pub(crate) fn notify_key(key: &str, seconds: u64) {
    notify_text(&i18n::tr(key), seconds);
}

/// 已构建好的安全文案（含参数）走同一单槽通知。
pub(crate) fn notify_text(message: &str, seconds: u64) {
    *NOTIFICATION.state().write() = Some(Notification {
        message: message.to_string(),
        until: peri_time::monotonic_now() + std::time::Duration::from_secs(seconds),
    });
}

/// 注销终态通知：`NOTIFICATION` 是单槽，后一条会覆盖前一条；把本地清理与
/// 远端撤销结果合并成一条，确保两部分结果都可见。
pub(crate) fn notify_logout_outcome(outcome: &LogoutOutcome) {
    let key = match (outcome.local_cleared, outcome.remote_revoked) {
        (true, true) => "login-auth-logout-cleared-remote-revoked",
        (true, false) => "login-auth-logout-cleared-remote-unconfirmed",
        (false, true) => "login-auth-logout-uncleared-remote-revoked",
        (false, false) => "login-auth-logout-uncleared-remote-unconfirmed",
    };
    notify_key(key, 3);
}
