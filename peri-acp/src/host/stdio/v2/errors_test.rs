//! Adapter-level failure surface: documented error codes and the host failure
//! mapping applied per request context.

use serde_json::json;

use super::harness::{expect_error, Harness, CWD};
use crate::transport::types::{AcpError, RequestId};

#[test]
fn host_failures_map_only_the_documented_codes() {
    use super::errors::{host_failure, HostContext};

    let busy = || AcpError::new(-32010, "Session is executing; retry after the current turn");
    let mapped = host_failure(
        busy(),
        HostContext::Setup {
            session_id: Some("A"),
        },
    );
    assert_eq!(mapped.code, -32020);
    assert_eq!(mapped.data.expect("reason")["reason"], "session_busy");

    // A setup cwd the host cannot use is the request's Invalid params, with
    // the host's diagnostic message intact.
    let mapped = host_failure(
        AcpError::new(-32010, "workspace location is unavailable"),
        HostContext::Setup {
            session_id: Some("A"),
        },
    );
    assert_eq!(mapped.code, -32602);
    assert_eq!(mapped.message, "workspace location is unavailable");

    // `session/new` has no session id, so the busy shape cannot be used.
    let mapped = host_failure(busy(), HostContext::Setup { session_id: None });
    assert_eq!(mapped.code, -32603);

    // Storage and prompt workspace failures are host-side, not client input.
    let mapped = host_failure(
        AcpError::new(-32010, "store offline"),
        HostContext::SessionList,
    );
    assert_eq!(mapped.code, -32603);
    assert_eq!(mapped.message, "store offline");
    let mapped = host_failure(
        AcpError::new(-32010, "workspace location is unavailable"),
        HostContext::Prompt,
    );
    assert_eq!(mapped.code, -32603);
    assert_eq!(mapped.message, "workspace location is unavailable");

    // The overloaded `-32000` keeps its documented remap; other codes pass.
    assert_eq!(
        host_failure(AcpError::new(-32000, "busy"), HostContext::Prompt).code,
        -32020
    );
    assert_eq!(
        host_failure(
            AcpError::new(-32603, "x"),
            HostContext::Setup { session_id: None }
        )
        .code,
        -32603
    );

    // Only the close-incomplete settlement keeps the host's `-32010`; every
    // other close failure is internal.
    let mapped = host_failure(
        AcpError::new(
            -32010,
            "Session close incomplete: active execution has not stopped",
        ),
        HostContext::Close,
    );
    assert_eq!(mapped.code, -32010);
    let mapped = host_failure(
        AcpError::new(-32010, "session store unavailable"),
        HostContext::Close,
    );
    assert_eq!(mapped.code, -32603);
    assert_eq!(mapped.message, "session store unavailable");
}

#[tokio::test]
async fn a_setup_with_an_unusable_cwd_is_invalid_params_not_the_host_code() {
    let mut harness = Harness::start();
    harness.negotiate().await;
    let id = RequestId::Number(2100);
    harness
        .request(
            id.clone(),
            "session/new",
            json!({"cwd": "/no/such/project"}),
        )
        .await;
    let forwarded = harness.expect_forwarded().await;
    assert_eq!(forwarded.method, "session/new");
    harness
        .respond(
            forwarded.id,
            Err(AcpError::new(-32010, "workspace location is unavailable")),
        )
        .await;
    let error = expect_error(&harness, id, -32602).await;
    assert_eq!(error.message, "workspace location is unavailable");
    harness.expect_quiet().await;
}

#[tokio::test]
async fn a_list_storage_failure_is_an_internal_error() {
    let mut harness = Harness::start();
    harness.negotiate().await;
    let id = RequestId::Number(2101);
    harness
        .request(id.clone(), "session/list", json!({"cwd": CWD}))
        .await;
    let forwarded = harness.expect_forwarded().await;
    assert_eq!(forwarded.method, super::SESSION_LIST);
    harness
        .respond(forwarded.id, Err(AcpError::new(-32010, "store offline")))
        .await;
    let error = expect_error(&harness, id, -32603).await;
    assert_eq!(error.message, "store offline");
    harness.expect_quiet().await;
}

#[tokio::test]
async fn a_prompt_workspace_failure_is_an_internal_error() {
    let mut harness = Harness::start();
    harness.negotiate().await;
    harness.open("A").await;
    let id = harness.request_prompt("A", "hello", None).await;
    let forwarded = harness.expect_forwarded().await;
    assert_eq!(forwarded.method, super::INPUT);
    harness
        .respond(
            forwarded.id,
            Err(AcpError::new(-32010, "workspace location is unavailable")),
        )
        .await;
    let error = expect_error(&harness, id, -32603).await;
    assert_eq!(error.message, "workspace location is unavailable");
    harness.expect_quiet().await;
}

#[tokio::test]
async fn only_the_close_incomplete_failure_keeps_the_host_conflict_code() {
    let mut harness = Harness::start();
    harness.negotiate().await;
    harness.open("A").await;

    let incomplete = RequestId::Number(2400);
    harness
        .request(
            incomplete.clone(),
            "session/close",
            json!({"sessionId": "A"}),
        )
        .await;
    let forwarded = harness.expect_forwarded().await;
    harness
        .respond(
            forwarded.id,
            Err(AcpError::new(
                -32010,
                "Session close incomplete: active execution has not stopped",
            )),
        )
        .await;
    let error = expect_error(&harness, incomplete, -32010).await;
    assert!(error.message.contains("close incomplete"), "{error}");

    // Any other close failure is a host-internal failure, not the retryable
    // close-incomplete settlement.
    let other = RequestId::Number(2401);
    harness
        .request(other.clone(), "session/close", json!({"sessionId": "A"}))
        .await;
    let forwarded = harness.expect_forwarded().await;
    harness
        .respond(
            forwarded.id,
            Err(AcpError::new(-32010, "session store unavailable")),
        )
        .await;
    let error = expect_error(&harness, other, -32603).await;
    assert_eq!(error.message, "session store unavailable");
    harness.expect_quiet().await;
}

#[tokio::test]
async fn unsupported_methods_and_object_cancels_use_the_documented_codes() {
    let mut harness = Harness::start();
    harness.negotiate().await;
    for (offset, (method, expected)) in [
        ("auth/login", -32601),
        ("auth/logout", -32601),
        ("session/fork", -32601),
        ("providers/list", -32601),
        ("mcp/message", -32601),
        ("terminal/cancel", -32602),
        ("tool_call/cancel", -32602),
        ("not/a/method", -32601),
    ]
    .into_iter()
    .enumerate()
    {
        let id = RequestId::Number(1000 + offset as i64);
        harness.request(id.clone(), method, json!({})).await;
        expect_error(&harness, id, expected).await;
    }
    // `-32000` means Authentication required in v2: host passthroughs that use
    // it for backpressure are rewritten instead of leaking through.
    assert_eq!(
        super::errors::normalize(AcpError::new(-32000, "busy")).code,
        -32020
    );
    assert_eq!(
        super::errors::normalize(AcpError::new(-32000, "busy"))
            .data
            .expect("reason")["reason"],
        "server_error"
    );
    assert_eq!(
        super::errors::normalize(AcpError::new(-32603, "x")).code,
        -32603
    );
}
