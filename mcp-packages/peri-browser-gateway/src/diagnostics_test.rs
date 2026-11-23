use serde_json::json;

use super::*;

#[test]
fn console_entries_join_arguments_and_keep_the_level() {
    let mut diagnostics = Diagnostics::new();
    diagnostics.observe(
        "Runtime.consoleAPICalled",
        &json!({
            "type": "warning",
            "timestamp": 1234.5,
            "args": [
                {"type": "string", "value": "hello"},
                {"type": "object", "description": "Object"}
            ]
        }),
    );

    let entries = diagnostics.console_entries();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].level, "warning");
    assert_eq!(entries[0].text, "hello Object");
    assert!((entries[0].timestamp_ms - 1234.5).abs() < f64::EPSILON);
}

#[test]
fn uncaught_exceptions_are_console_errors() {
    let mut diagnostics = Diagnostics::new();
    diagnostics.observe(
        "Runtime.exceptionThrown",
        &json!({
            "exceptionDetails": {
                "text": "Uncaught",
                "timestamp": 9.0,
                "exception": {"description": "TypeError: x is not a function"}
            }
        }),
    );

    let entries = diagnostics.console_entries();
    assert_eq!(entries[0].level, "error");
    assert_eq!(entries[0].text, "Uncaught: TypeError: x is not a function");
}

#[test]
fn a_request_is_enriched_by_its_response_and_completion() {
    let mut diagnostics = Diagnostics::new();
    diagnostics.observe(
        "Network.requestWillBeSent",
        &json!({
            "requestId": "r1",
            "type": "XHR",
            "timestamp": 1.0,
            "request": {
                "url": "https://api.test/items",
                "method": "GET",
                "headers": {"accept": "application/json"}
            }
        }),
    );
    let pending = diagnostics.get_network("r1").expect("buffered");
    assert_eq!(pending.status, None);
    assert!(!pending.finished);
    assert_eq!(pending.method, "GET");
    assert_eq!(pending.resource_type.as_deref(), Some("XHR"));

    diagnostics.observe(
        "Network.responseReceived",
        &json!({
            "requestId": "r1",
            "type": "XHR",
            "response": {
                "status": 200,
                "statusText": "OK",
                "mimeType": "application/json",
                "headers": {"content-type": "application/json"}
            }
        }),
    );
    diagnostics.observe(
        "Network.loadingFinished",
        &json!({"requestId": "r1", "timestamp": 2.0}),
    );

    let finished = diagnostics.get_network("r1").expect("still buffered");
    assert_eq!(finished.status, Some(200));
    assert_eq!(finished.status_text.as_deref(), Some("OK"));
    assert_eq!(finished.mime_type.as_deref(), Some("application/json"));
    assert!(finished.finished);
    assert!(finished.response_headers.is_some());
}

#[test]
fn a_failed_request_keeps_the_failure_reason() {
    let mut diagnostics = Diagnostics::new();
    diagnostics.observe(
        "Network.requestWillBeSent",
        &json!({
            "requestId": "r2",
            "request": {"url": "https://api.test/boom", "method": "POST"}
        }),
    );
    diagnostics.observe(
        "Network.loadingFailed",
        &json!({"requestId": "r2", "errorText": "net::ERR_FAILED", "timestamp": 3.0}),
    );

    let failed = diagnostics.get_network("r2").expect("buffered");
    assert_eq!(failed.error.as_deref(), Some("net::ERR_FAILED"));
    assert!(failed.finished);
}

#[test]
fn network_listing_filters_by_url_and_returns_newest_first() {
    let mut diagnostics = Diagnostics::new();
    for (id, url) in [
        ("a", "https://one.test/api"),
        ("b", "https://two.test/page"),
        ("c", "https://two.test/api"),
    ] {
        diagnostics.observe(
            "Network.requestWillBeSent",
            &json!({"requestId": id, "request": {"url": url, "method": "GET"}}),
        );
    }

    let all = diagnostics.list_network(None);
    assert_eq!(all.len(), 3);
    assert_eq!(all[0].request_id, "c");
    assert_eq!(all[2].request_id, "a");

    let filtered = diagnostics.list_network(Some("two.test/api"));
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].request_id, "c");

    assert!(diagnostics.list_network(Some("nothing")).is_empty());
}

#[test]
fn diagnostics_are_bounded_and_oldest_entries_are_dropped() {
    let mut diagnostics = Diagnostics::new();
    for index in 0..DIAGNOSTIC_CAPACITY + 25 {
        diagnostics.observe(
            "Runtime.consoleAPICalled",
            &json!({"type": "log", "args": [{"value": index.to_string()}]}),
        );
    }
    let entries = diagnostics.console_entries();
    assert_eq!(entries.len(), DIAGNOSTIC_CAPACITY);
    assert_eq!(entries[0].text, "25");
    assert_eq!(
        entries.last().unwrap().text,
        (DIAGNOSTIC_CAPACITY + 24).to_string()
    );
}

#[test]
fn domain_enablement_is_tracked_once() {
    let mut diagnostics = Diagnostics::new();
    assert!(!diagnostics.runtime_enabled());
    assert!(!diagnostics.network_enabled());
    diagnostics.mark_runtime_enabled();
    diagnostics.mark_network_enabled();
    assert!(diagnostics.runtime_enabled());
    assert!(diagnostics.network_enabled());
    diagnostics.clear();
    assert!(diagnostics.console().is_empty());
    assert!(diagnostics.network().is_empty());
}

#[test]
fn unknown_events_are_ignored() {
    let mut diagnostics = Diagnostics::new();
    diagnostics.observe("Page.loadEventFired", &json!({}));
    assert!(diagnostics.console_entries().is_empty());
    assert!(diagnostics.list_network(None).is_empty());
}
