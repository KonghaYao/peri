//! Tests for router_hub

use super::*;

#[test]
fn test_has_session_empty() {
    let (tx, _rx) = mpsc::unbounded_channel();
    let router = SessionRouter::new(vec!["echo".into()], tx, 10, 300);
    assert!(!router.has_session("nonexistent"));
}

#[test]
fn test_list_sessions_empty() {
    let (tx, _rx) = mpsc::unbounded_channel();
    let router = SessionRouter::new(vec!["echo".into()], tx, 10, 300);
    assert!(router.list_sessions().is_empty());
}
