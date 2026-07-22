//! Tests for oauth_flow

use super::*;

#[test]
fn test_oauth_flow_error_display() {
    let err = OAuthFlowError::Cancelled;
    assert!(err.to_string().contains("取消"));
}

#[test]
fn test_oauth_flow_event_types() {
    let event1 = OAuthFlowEvent::AuthorizationNeeded {
        server_name: "srv".to_string(),
        authorization_url: "http://example.com".to_string(),
        callback_tx: oneshot::channel().0,
    };
    if let OAuthFlowEvent::AuthorizationNeeded { server_name, .. } = event1 {
        assert_eq!(server_name, "srv");
    }

    let event2 = OAuthFlowEvent::AuthorizationCompleted {
        server_name: "srv".to_string(),
    };
    if let OAuthFlowEvent::AuthorizationCompleted { server_name, .. } = event2 {
        assert_eq!(server_name, "srv");
    }

    let event3 = OAuthFlowEvent::AuthorizationFailed {
        server_name: "srv".to_string(),
        error: "fail".to_string(),
    };
    if let OAuthFlowEvent::AuthorizationFailed { error, .. } = event3 {
        assert_eq!(error, "fail");
    }
}

#[test]
fn test_oauth_callback_result_fields() {
    let result = OAuthCallbackResult {
        code: "abc".to_string(),
        state: "xyz".to_string(),
    };
    assert_eq!(result.code, "abc");
    assert_eq!(result.state, "xyz");
}

#[tokio::test]
async fn test_oauth_flow_manager_new() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    drop(tmp);
    let store = Arc::new(FileCredentialStore::with_path(path));
    let manager = OAuthFlowManager::new(store, |_| {});
    assert!(!manager.is_authorized("nonexistent"));
}

#[tokio::test]
async fn test_oauth_flow_manager_is_authorized_empty() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    drop(tmp);
    let store = Arc::new(FileCredentialStore::with_path(path));
    let manager = OAuthFlowManager::new(store, |_| {});
    assert!(!manager.is_authorized("any-server"));
}

#[tokio::test]
async fn test_oauth_flow_manager_emit_event() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let counter = Arc::new(AtomicUsize::new(0));
    let counter_clone = counter.clone();

    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    drop(tmp);
    let store = Arc::new(FileCredentialStore::with_path(path));
    let manager = OAuthFlowManager::new(store, move |_| {
        counter_clone.fetch_add(1, Ordering::SeqCst);
    });

    manager.emit_event(OAuthFlowEvent::AuthorizationCompleted {
        server_name: "test".to_string(),
    });
    assert_eq!(counter.load(Ordering::SeqCst), 1);
}
