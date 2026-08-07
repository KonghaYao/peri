use super::*;
use peri_agent::agent::events::{MiddlewareHook, StageStatus};

#[test]
fn test_on_start_returns_handle() {
    let mut m = MiddlewareTracer::new();
    let h = m.on_start("HookMW", MiddlewareHook::BeforeAgent);
    assert!(h.span_id.starts_with("span_"));
}

#[test]
fn test_on_end_returns_stats() {
    let mut m = MiddlewareTracer::new();
    let h = m.on_start("HookMW", MiddlewareHook::BeforeAgent);
    let end = m
        .on_end(&h, StageStatus::Done, None)
        .expect("should return Some");
    assert_eq!(end.name, "HookMW");
    assert_eq!(end.status, StageStatus::Done);
}

#[test]
fn test_on_end_unknown_returns_none() {
    let mut m = MiddlewareTracer::new();
    let h = MiddlewareSpanHandle {
        span_id: "unknown".into(),
        name: "X".into(),
        hook: MiddlewareHook::BeforeAgent,
    };
    assert!(m.on_end(&h, StageStatus::Done, None).is_none());
}

#[test]
fn test_concurrent_same_hook_preserves_pairing() {
    let mut m = MiddlewareTracer::new();
    let h1 = m.on_start("MW1", MiddlewareHook::BeforeAgent);
    let h2 = m.on_start("MW2", MiddlewareHook::BeforeAgent);
    assert!(m.on_end(&h1, StageStatus::Done, None).is_some());
    assert!(m.on_end(&h2, StageStatus::Done, None).is_some());
}

#[test]
fn test_on_end_with_error_carries_message() {
    let mut m = MiddlewareTracer::new();
    let h = m.on_start("FailingMW", MiddlewareHook::AfterTool);
    let end = m
        .on_end(&h, StageStatus::Error, Some("panic".into()))
        .unwrap();
    assert_eq!(end.error.as_deref(), Some("panic"));
}
