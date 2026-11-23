//! URL elicitation 端口：池内投递与「无宿主端口即 decline」的归属语义。

use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use super::{
    UrlElicitationOutcome, UrlElicitationPort, UrlElicitationRequest, UrlElicitationSource,
};
use crate::mcp::client::McpClientPool;

struct RecordingPort {
    outcome: UrlElicitationOutcome,
    calls: AtomicUsize,
    seen: parking_lot::Mutex<Vec<UrlElicitationRequest>>,
}

#[async_trait::async_trait]
impl UrlElicitationPort for RecordingPort {
    async fn elicit_url(&self, request: UrlElicitationRequest) -> UrlElicitationOutcome {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.seen.lock().push(request);
        self.outcome
    }
}

fn port(outcome: UrlElicitationOutcome) -> Arc<RecordingPort> {
    Arc::new(RecordingPort {
        outcome,
        calls: AtomicUsize::new(0),
        seen: parking_lot::Mutex::new(Vec::new()),
    })
}

fn request() -> UrlElicitationRequest {
    UrlElicitationRequest {
        server_name: "peri-browser".into(),
        message: "Authorize the browser session".into(),
        url: "https://example.test/authorize?state=abc".into(),
        elicitation_id: "e-1".into(),
    }
}

#[tokio::test]
async fn pool_forwards_url_elicitation_to_the_installed_port() {
    let pool = Arc::new(McpClientPool::new_pending());
    let recorder = port(UrlElicitationOutcome::Accepted);
    pool.set_url_elicitation_port(recorder.clone());

    let outcome = pool.elicit_url(request()).await;

    assert_eq!(outcome, UrlElicitationOutcome::Accepted);
    assert_eq!(recorder.calls.load(Ordering::SeqCst), 1);
    let seen = recorder.seen.lock();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].server_name, "peri-browser");
    assert_eq!(seen[0].url, "https://example.test/authorize?state=abc");
    assert_eq!(seen[0].elicitation_id, "e-1");
}

#[tokio::test]
async fn pool_without_a_port_declines_instead_of_guessing_an_endpoint() {
    let pool = Arc::new(McpClientPool::new_pending());

    assert_eq!(
        pool.elicit_url(request()).await,
        UrlElicitationOutcome::Declined
    );
}

#[tokio::test]
async fn connection_source_resolves_the_port_through_its_pool() {
    let pool = Arc::new(McpClientPool::new_pending());
    let recorder = port(UrlElicitationOutcome::Declined);
    pool.set_url_elicitation_port(recorder.clone());
    let source = UrlElicitationSource::new(Arc::downgrade(&pool), "peri-browser");

    let outcome = source
        .elicit(
            "Authorize".into(),
            "https://example.test/a".into(),
            "e-2".into(),
        )
        .await;

    assert_eq!(outcome, UrlElicitationOutcome::Declined);
    assert_eq!(recorder.calls.load(Ordering::SeqCst), 1);
    assert_eq!(recorder.seen.lock()[0].server_name, "peri-browser");
}

#[tokio::test]
async fn released_pool_declines_without_calling_a_stale_port() {
    let pool = Arc::new(McpClientPool::new_pending());
    let recorder = port(UrlElicitationOutcome::Accepted);
    pool.set_url_elicitation_port(recorder.clone());
    let source = UrlElicitationSource::new(Arc::downgrade(&pool), "peri-browser");
    drop(pool);

    assert_eq!(
        source
            .elicit(
                "Authorize".into(),
                "https://example.test/a".into(),
                "e-3".into()
            )
            .await,
        UrlElicitationOutcome::Declined
    );
    assert_eq!(recorder.calls.load(Ordering::SeqCst), 0);
}
