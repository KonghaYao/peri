use super::*;
use peri_agent::agent::events::{CompactStrategy, CompactTrigger};

#[test]
fn test_initial_state_inactive() {
    let c = CompactSpan::new();
    assert!(!c.is_active());
}

#[test]
fn test_on_start_activates() {
    let mut c = CompactSpan::new();
    let start = c.on_start(CompactStrategy::Full, CompactTrigger::Auto);
    assert!(start.span_id.starts_with("span_"));
    assert!(c.is_active());
}

#[test]
fn test_on_end_returns_context() {
    let mut c = CompactSpan::new();
    c.on_start(CompactStrategy::Micro, CompactTrigger::Auto);
    let ctx = c.on_end().expect("should return Some");
    assert!(ctx.span_id.starts_with("span_"));
    assert!(!c.is_active());
}

#[test]
fn test_on_end_without_start_returns_none() {
    let mut c = CompactSpan::new();
    assert!(c.on_end().is_none());
}

#[test]
fn test_double_start_overwrites() {
    let mut c = CompactSpan::new();
    c.on_start(CompactStrategy::Micro, CompactTrigger::Auto);
    c.on_start(CompactStrategy::Full, CompactTrigger::Manual);
    let ctx = c.on_end().unwrap();
    assert_eq!(ctx.strategy, CompactStrategy::Full);
    assert_eq!(ctx.trigger, CompactTrigger::Manual);
}
