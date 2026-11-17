//! Host-side ChatGPT authentication service boundary.
//!
//! The TUI never reads or writes credentials itself. This module exposes a small
//! typed capability boundary over `peri_acp::provider::auth::ChatGptAuthManager`
//! so panels can be tested with a fake backend, plus the shared credential-store
//! path rule used by both `peri auth chatgpt` and `/login`.
//!
//! No token, authorization code or URL is logged here.

mod actions;
mod controller;
mod dispatch;

mod backend;
pub use backend::*;

pub(crate) use actions::{copy_url, notify_key, notify_logout_outcome, notify_text, open_browser};
#[cfg(test)]
pub(crate) use actions::{take_test_dispatches, take_test_url_actions};
#[cfg(test)]
pub(crate) use controller::AuthEvent;
pub(crate) use controller::{
    AuthAction, AuthController, AuthEffect, AuthFailure, AuthViewState, DetailAction,
    HostConfigAuthScope, LogoutConfirm, LogoutFocus,
};
pub(crate) use dispatch::{dispatch_login, dispatch_logout, dispatch_status};
pub(crate) use peri_acp::provider::auth::AccountStatus;
#[cfg(test)]
pub(crate) use peri_acp::provider::auth::LogoutOutcome;
