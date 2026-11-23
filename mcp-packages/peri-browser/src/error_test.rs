use crate::cdp::CdpError;
use serde_json::json;

use super::*;

#[test]
fn every_local_variant_has_a_stable_code() {
    assert_eq!(BrowserError::NotConnected.code(), "not_connected");
    assert_eq!(BrowserError::NoActivePage.code(), "no_active_page");
    assert_eq!(
        BrowserError::PageNotFound("p1".to_owned()).code(),
        "page_not_found"
    );
    assert_eq!(
        BrowserError::AmbiguousPage("p".to_owned()).code(),
        "ambiguous_page"
    );
    assert_eq!(
        BrowserError::InvalidArgument {
            name: "uid".to_owned(),
            reason: "missing".to_owned(),
        }
        .code(),
        "invalid_argument"
    );
    assert_eq!(
        BrowserError::GatewayUnavailable("down".to_owned()).code(),
        "gateway_unavailable"
    );
    assert_eq!(BrowserError::WaitingForHuman.code(), "waiting_for_human");
    assert_eq!(BrowserError::Yielded.code(), "page_busy_by_human");
    assert_eq!(BrowserError::StaleUid("1_0".to_owned()).code(), "stale_uid");
    assert_eq!(
        BrowserError::ElementNotFound("#go".to_owned()).code(),
        "element_not_found"
    );
    assert_eq!(
        BrowserError::NotVisible("7".to_owned()).code(),
        "element_not_visible"
    );
    assert_eq!(BrowserError::Timeout("slow".to_owned()).code(), "timeout");
    assert_eq!(
        BrowserError::LockTimeout(std::path::PathBuf::from("/tmp/x")).code(),
        "provision_lock_timeout"
    );
    assert_eq!(
        BrowserError::BrowserControl {
            method: "Target.createTarget".to_owned(),
            message: "no id".to_owned(),
        }
        .code(),
        "browser_control"
    );
    assert_eq!(
        BrowserError::Script("boom".to_owned()).code(),
        "script_error"
    );
    assert_eq!(
        BrowserError::Internal("bad".to_owned()).code(),
        "internal_error"
    );
}

#[test]
fn kitesurf_protocol_errors_map_to_actionable_codes() {
    let cases = [
        (
            "Another controller owns this page session",
            "another_controller",
        ),
        (
            "Target belongs to another page session",
            "target_owned_elsewhere",
        ),
        (
            "This method is not available on a page-session connection",
            "method_not_available",
        ),
        ("Control has changed", "control_changed"),
        (
            "Viewer connection cannot control the browser",
            "viewer_denied",
        ),
        ("Target expired", "target_expired"),
        ("something else", "cdp_error"),
    ];
    for (message, expected) in cases {
        let error = BrowserError::from(CdpError::Rejected {
            method: "Page.navigate".to_owned(),
            message: message.to_owned(),
        });
        assert_eq!(error.code(), expected, "message {message:?}");
    }
}

#[test]
fn transport_failures_have_their_own_codes() {
    let timeout = BrowserError::from(CdpError::Timeout {
        method: "Runtime.evaluate".to_owned(),
        timeout_ms: 25_000,
    });
    assert_eq!(timeout.code(), "egress_timeout");
    assert!(timeout.hint().is_some_and(|hint| hint.contains("retry")));

    let closed = BrowserError::from(CdpError::Closed {
        url: "ws://127.0.0.1:1/devtools/page/p1".to_owned(),
    });
    assert_eq!(closed.code(), "cdp_closed");
    assert!(closed.hint().is_some_and(|hint| hint.contains("target")));

    let stale = BrowserError::from(CdpError::Protocol {
        code: -32000,
        message: "Execution context was destroyed".to_owned(),
    });
    assert_eq!(stale.code(), "context_destroyed");

    let invalid = BrowserError::from(CdpError::InvalidUrl {
        url: "http://127.0.0.1:1".to_owned(),
        reason: "expected ws".to_owned(),
    });
    assert_eq!(invalid.code(), "cdp_invalid_url");
}

#[test]
fn render_prefixes_the_code_and_appends_the_hint() {
    let rendered = BrowserError::NotConnected.render();
    assert!(rendered.starts_with("not_connected: "), "{rendered}");
    assert!(rendered.contains("KITESURF_CDP_URL"), "{rendered}");

    let no_hint = BrowserError::InvalidArgument {
        name: "uid".to_owned(),
        reason: "missing".to_owned(),
    }
    .render();
    assert_eq!(no_hint, "invalid_argument: invalid argument uid: missing");
}

#[test]
fn tool_errors_serialize_code_and_hint_together() {
    let error = BrowserError::StaleUid("2_1".to_owned());
    let payload = json!({"code": error.code(), "hint": error.hint()});
    assert_eq!(payload["code"], "stale_uid");
    assert!(payload["hint"].as_str().unwrap().contains("take_snapshot"));
}
