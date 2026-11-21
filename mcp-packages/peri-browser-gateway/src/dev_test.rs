use axum::routing::get;
use axum::{Json, Router};
use serde_json::json;
use tokio::net::TcpListener;

use super::*;

const TARGETS: &str = r#"[
    {
        "id": "t1",
        "type": "page",
        "title": "Example Domain",
        "url": "https://example.org/",
        "webSocketDebuggerUrl": "ws://127.0.0.1:8827/devtools/page/t1"
    },
    {
        "id": "t2",
        "type": "page",
        "title": "",
        "url": "about:blank"
    }
]"#;

#[test]
fn parses_page_targets_in_order() {
    let pages = parse_targets(TARGETS).expect("valid target list");
    assert_eq!(
        pages,
        vec![
            PageRef {
                target_id: "t1".to_owned(),
                url: "https://example.org/".to_owned(),
                title: "Example Domain".to_owned(),
            },
            PageRef {
                target_id: "t2".to_owned(),
                url: "about:blank".to_owned(),
                title: String::new(),
            },
        ]
    );
}

#[test]
fn skips_non_page_and_idless_targets() {
    let body = json!([
        {"id": "worker", "type": "service_worker", "url": "https://example.test/sw.js"},
        {"id": "", "type": "page", "url": "about:blank"},
        {"id": "page", "title": "Kept", "url": "https://example.test/"}
    ])
    .to_string();

    let pages = parse_targets(&body).expect("valid target list");
    assert_eq!(pages.len(), 1);
    assert_eq!(pages[0].target_id, "page");
    assert_eq!(pages[0].title, "Kept");
}

#[test]
fn rejects_a_non_array_payload() {
    let error = parse_targets(r#"{"targets":[]}"#).expect_err("only an array is accepted");
    assert!(error.is_data());
}

#[tokio::test]
async fn fetch_pages_reads_the_http_endpoint() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("local address");
    let app = Router::new().route(
        "/json/list",
        get(|| async {
            Json(json!([{"id": "t1", "type": "page", "title": "T", "url": "about:blank"}]))
        }),
    );
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });

    let pages = fetch_pages(&Client::new(), &format!("http://{address}/json/list"))
        .await
        .expect("target list is fetched");
    server.abort();

    assert_eq!(
        pages,
        vec![PageRef {
            target_id: "t1".to_owned(),
            url: "about:blank".to_owned(),
            title: "T".to_owned(),
        }]
    );
}
