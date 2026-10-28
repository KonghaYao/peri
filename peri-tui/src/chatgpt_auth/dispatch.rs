//! 认证任务只投递安全 DTO，界面在 effect 边界经完成门接收。

use super::controller::*;
use super::controller::{queue_into, runtime_handle};
use ratatui_kit::prelude::State;

pub(crate) fn dispatch_login(state: State<AuthController>) -> bool {
    let request = match state.write().prepare_login() {
        Ok(request) => request,
        Err(_) => return false,
    };
    #[cfg(test)]
    if super::actions::intercept_dispatch("login") {
        return true;
    }
    let Some(handle) = runtime_handle() else {
        state.write().abort_unspawned(AuthFailure::Unavailable);
        return false;
    };
    let inbox = state.read().inbox.clone();
    let AuthRequest { ticket, session } = request;
    let url_inbox = inbox.clone();
    let url_ticket = ticket.clone();
    let on_url = Box::new(move |url: String| {
        queue_into(&url_inbox, url_ticket, AuthEvent::AuthorizationUrl(url));
    });
    handle.spawn(async move {
        let result = session.login(ticket.token().clone(), on_url).await;
        queue_into(&inbox, ticket, AuthEvent::LoginFinished(result));
    });
    true
}

pub(crate) fn dispatch_status(state: State<AuthController>, reconcile: bool) -> bool {
    let request = match state.write().prepare_status(reconcile) {
        Some(request) => request,
        None => return false,
    };
    #[cfg(test)]
    if super::actions::intercept_dispatch(if reconcile {
        "status-reconcile"
    } else {
        "status"
    }) {
        return true;
    }
    let Some(handle) = runtime_handle() else {
        state.write().abort_unspawned(AuthFailure::Unavailable);
        return false;
    };
    let inbox = state.read().inbox.clone();
    handle.spawn(async move {
        let result = request.session.status(request.ticket.token().clone()).await;
        queue_into(&inbox, request.ticket, AuthEvent::StatusChecked(result));
    });
    true
}

pub(crate) fn dispatch_logout(state: State<AuthController>) -> bool {
    let request = match state.write().prepare_logout() {
        Some(request) => request,
        None => return false,
    };
    #[cfg(test)]
    if super::actions::intercept_dispatch("logout") {
        return true;
    }
    let Some(handle) = runtime_handle() else {
        state.write().abort_unspawned(AuthFailure::Unavailable);
        return false;
    };
    let inbox = state.read().inbox.clone();
    let observer = state.read().observer.clone();
    handle.spawn(async move {
        let result = request.session.logout(request.ticket.token().clone()).await;
        if !observer.load(std::sync::atomic::Ordering::SeqCst) {
            match &result {
                Ok(outcome) => super::notify_logout_outcome(outcome),
                Err(_) => {
                    tracing::warn!("ChatGPT logout finished after the panel closed");
                    super::notify_key("login-auth-outcome-unknown", 3);
                }
            }
        }
        queue_into(&inbox, request.ticket, AuthEvent::LoggedOut(result));
    });
    true
}
