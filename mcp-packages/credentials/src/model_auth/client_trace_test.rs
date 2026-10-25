//! Exercise the real credentials MCP transport with DEBUG/TRACE enabled. No
//! production credentials or external network are used by this regression.
use std::{fmt, sync::atomic::AtomicU64};

use tracing::{
    field::{Field, Visit},
    level_filters::LevelFilter,
    span::{Attributes, Id, Record},
    Event, Metadata, Subscriber,
};

use super::*;

#[derive(Clone)]
struct Capture {
    level: LevelFilter,
    text: Arc<Mutex<String>>,
    next_span: Arc<AtomicU64>,
}

struct Fields<'a>(&'a mut String);

impl Visit for Fields<'_> {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        use fmt::Write;
        write!(self.0, " {}={value:?}", field.name()).unwrap();
    }
}

impl Subscriber for Capture {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.level() <= &self.level
    }

    fn max_level_hint(&self) -> Option<LevelFilter> {
        Some(self.level)
    }

    fn new_span(&self, _: &Attributes<'_>) -> Id {
        Id::from_u64(self.next_span.fetch_add(1, Ordering::Relaxed))
    }

    fn record(&self, _: &Id, _: &Record<'_>) {}
    fn record_follows_from(&self, _: &Id, _: &Id) {}
    fn enter(&self, _: &Id) {}
    fn exit(&self, _: &Id) {}

    fn event(&self, event: &Event<'_>) {
        if self.enabled(event.metadata()) {
            let mut text = self.text.lock().unwrap();
            event.record(&mut Fields(&mut text));
            text.push('\n');
        }
    }
}

fn check_mcp_logs(level: LevelFilter) {
    let capture = Capture {
        level,
        text: Arc::new(Mutex::new(String::new())),
        next_span: Arc::new(AtomicU64::new(1)),
    };
    let _subscriber = tracing::subscriber::set_default(capture.clone());
    // All spawned MCP tasks run under this thread's subscriber, without a
    // process-global tracing installation that would affect other unit tests.
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let mock = MockIssuer::start().await;
            let dir = tempfile::tempdir().unwrap();
            let manager = mock.manager(dir.path(), "trace-fixture");
            seed(&manager, 5000).await;
            let credential = manager.credential(CancellationToken::new()).await.unwrap();
            assert_eq!(credential.bearer(), "synthetic-access-initial");

            // Business Err is carried by a successful CustomResult. Its server
            // boundary must still preserve the actual OAuth code/description.
            *mock.state.mode.lock().unwrap() = Mode::InvalidGrant;
            let error = manager
                .refresh_after_unauthorized(&credential, CancellationToken::new())
                .await
                .unwrap_err();
            assert!(error.to_string().contains("invalid_grant"));
            assert!(error
                .to_string()
                .contains("synthetic OAuth provider diagnosis"));

            // A JSON-RPC Error follows a different rmcp branch; verify that
            // removing successful payload logging did not silence this path.
            let (client_io, server_io) = tokio::io::duplex(64 * 1024);
            let server = Capability(manager.server.clone());
            let server_task = tokio::spawn(async move {
                server
                    .serve(server_io)
                    .await
                    .unwrap()
                    .waiting()
                    .await
                    .unwrap();
            });
            let mut client = ().serve(client_io).await.unwrap();
            let handle = client
                .peer()
                .send_request_with_option(
                    ClientRequest::CustomRequest(CustomRequest::new(
                        METHOD,
                        Some(json!({"invalid_operation":true})),
                    )),
                    PeerRequestOptions::no_options(),
                )
                .await
                .unwrap();
            assert!(handle
                .await_response()
                .await
                .unwrap_err()
                .to_string()
                .contains("Invalid auth operation"));
            let _ = client.close_with_timeout(Duration::from_secs(1)).await;
            peri_time::timeout(Duration::from_secs(2), server_task)
                .await
                .unwrap()
                .unwrap();
        });

    let logs = capture.text.lock().unwrap();
    assert!(
        logs.contains("peri/chatgpt_auth"),
        "real MCP request was not logged"
    );
    assert!(
        logs.contains("response message"),
        "successful response metadata was not logged"
    );
    for token in [
        "synthetic-access-initial",
        "synthetic-refresh-initial",
        "synthetic-stored-id-token",
    ] {
        assert!(
            !logs.contains(token),
            "successful credential payload was logged at {level}"
        );
    }
    assert!(logs.contains("invalid_grant"));
    assert!(logs.contains("synthetic OAuth provider diagnosis"));
    assert_eq!(
        logs.matches("ChatGPT authentication operation failed")
            .count(),
        1
    );
    assert!(logs.contains("response error"));
    assert!(logs.contains("Invalid auth operation"));
    if level == LevelFilter::TRACE {
        assert!(logs.contains("direction=\"incoming\""));
        assert!(logs.contains("direction=\"outgoing\""));
    }
}

#[test]
fn credentials_mcp_debug_omits_success_tokens_but_keeps_errors() {
    check_mcp_logs(LevelFilter::DEBUG);
}

#[test]
fn credentials_mcp_trace_omits_success_tokens_but_keeps_errors() {
    check_mcp_logs(LevelFilter::TRACE);
}
