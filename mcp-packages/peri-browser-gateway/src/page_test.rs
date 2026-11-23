use serde_json::json;

use super::*;
use crate::config::Config;
use crate::snapshot::build_ax_snapshot;

fn page() -> Arc<PageHandle> {
    let config =
        Config::parse_from([], |key| (key == "HOME").then(|| "/fixture".to_owned())).unwrap();
    PageHandle::new(
        "target-1".to_owned(),
        "ws://127.0.0.1:1/devtools/browser".to_owned(),
        &config.browser,
        watch::channel(0).0,
        CancellationToken::new(),
    )
}

#[test]
fn controller_owner_follows_the_engine_controller_not_the_page_id() {
    let page = page();
    assert_eq!(page.controller_owner, "peri-browser");
    assert_ne!(page.controller_owner, page.id);
}

#[tokio::test]
async fn navigation_events_keep_only_a_title_that_matches_the_top_level_url() {
    let page = page();
    page.set_navigation(&NavigationState {
        url: "https://example.test/".to_owned(),
        title: "Example".to_owned(),
        can_go_back: true,
        can_go_forward: false,
    })
    .await;
    for (frame, title, url) in [
        (
            json!({"url": "https://example.test/"}),
            "Example",
            "https://example.test/",
        ),
        (
            json!({"url": "https://iframe.test/", "parentId": "top"}),
            "Example",
            "https://example.test/",
        ),
        (
            json!({"url": "https://example.test/next"}),
            "",
            "https://example.test/next",
        ),
    ] {
        page.handle_event(CdpEvent {
            method: "Page.frameNavigated".to_owned(),
            params: json!({"frame": frame}),
        })
        .await;
        let meta = page.meta().await;
        assert_eq!(meta.url, url);
        assert_eq!(meta.title, title);
        assert!(meta.can_go_back);
        assert!(!meta.can_go_forward);
    }
}

#[tokio::test]
async fn page_identity_is_not_the_target_and_a_fresh_page_is_free() {
    let first = page();
    let other = page();
    assert_eq!(first.target_id, other.target_id);
    assert_ne!(first.id, other.id);
    assert_ne!(first.id, first.target_id);
    assert!(first.id.starts_with("p_"));
    assert_eq!(first.meta().await.url, "about:blank");
    assert!(first.meta().await.title.is_empty());
    assert_eq!(first.control.lease().role, Role::Free);
    let book = first.snapshot.lock().await;
    assert_eq!(book.generation(), 0);
    assert!(book.is_empty());
}

#[tokio::test]
async fn document_changes_clear_uids_and_expired_target_fences_the_lease() {
    let page = page();
    let nodes = [json!({"nodeId": "1", "role": {"value": "button"}, "backendDOMNodeId": 42})];
    for method in [
        "DOM.documentUpdated",
        "Runtime.executionContextsCleared",
        "Peri.targetExpired",
    ] {
        let mut book = page.snapshot.lock().await;
        let snapshot = build_ax_snapshot(&nodes, book.generation() + 1);
        let uid = snapshot.uids.keys().next().unwrap();
        book.store(&snapshot);
        assert_eq!(book.resolve(uid).unwrap(), 42);
        drop(book);
        page.handle_event(CdpEvent {
            method: method.to_owned(),
            params: json!({}),
        })
        .await;
        let book = page.snapshot.lock().await;
        assert!(!book.is_valid());
        assert!(matches!(
            book.resolve(uid),
            Err(crate::error::BrowserError::StaleUid(_))
        ));
    }
    assert!(page.expired());
    assert_eq!(page.control.lease().role, Role::Closing);
}
