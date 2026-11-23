use crate::cdp::{CdpEvent, NavigationState};
use serde_json::json;

use super::*;

#[tokio::test]
async fn navigation_events_keep_a_title_that_still_matches_the_url() {
    let page = PageHandle::new("target-0");
    page.set_meta("https://example.test/", "Example").await;

    page.handle_event(CdpEvent {
        method: "Page.frameNavigated".to_owned(),
        params: json!({"frame": {"url": "https://example.test/"}}),
    })
    .await;
    let meta = page.meta().await;
    assert_eq!(meta.url, "https://example.test/");
    assert_eq!(meta.title, "Example");

    page.handle_event(CdpEvent {
        method: "Page.frameNavigated".to_owned(),
        params: json!({"frame": {"url": "https://example.test/next"}}),
    })
    .await;
    let meta = page.meta().await;
    assert_eq!(meta.url, "https://example.test/next");
    assert!(meta.title.is_empty());
}

#[tokio::test]
async fn page_meta_is_shared_and_mutable() {
    let page = PageHandle::new("target-1");
    assert_eq!(page.id, "target-1");
    assert_eq!(page.target_id, "target-1");
    let initial = page.meta().await;
    assert_eq!(initial.url, "about:blank");
    assert!(initial.title.is_empty());

    page.set_meta("https://example.test/", "Example").await;
    let meta = page.meta().await;
    assert_eq!(meta.url, "https://example.test/");
    assert_eq!(meta.title, "Example");
}

#[tokio::test]
async fn yielding_is_reversible_by_a_reclaim() {
    let page = PageHandle::new("target-2");
    assert!(!page.is_yielded());
    page.mark_yielded(true);
    assert!(page.is_yielded());
    page.notify_reclaimed();
    assert!(!page.is_yielded());
}

#[tokio::test]
async fn a_fresh_page_has_no_controller_and_no_snapshot() {
    let page = PageHandle::new("target-3");
    assert!(!page.has_controller().await);
    assert_eq!(page.snapshot_generation().await, 0);
    page.invalidate_snapshot().await;
    assert_eq!(page.snapshot_generation().await, 1);
}

#[test]
fn navigation_snapshot_projects_url_and_title_only() {
    let value = navigation_snapshot(&NavigationState {
        url: "https://example.test/next".to_owned(),
        title: "Next".to_owned(),
        can_go_back: true,
        can_go_forward: false,
    });
    assert_eq!(
        value,
        json!({"url": "https://example.test/next", "title": "Next"})
    );
}

#[test]
fn content_scripts_are_single_statements() {
    assert!(SELECT_ALL.contains("document.activeElement"));
    assert!(SELECT_ALL.contains("selectionStart"));
    assert!(NOTIFY_EDIT.contains("new Event('input'"));
    assert!(NOTIFY_EDIT.contains("bubbles: true"));
}
