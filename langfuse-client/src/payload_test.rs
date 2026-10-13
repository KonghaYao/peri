//! `BudgetedBytes` 与 `ExportPayload` 的所有权契约测试（契约 §3.3、§4）。
//!
//! 覆盖：正文不得超过 permit、共享 `Arc` 时 permit 与 `Bytes` 同寿、Drop 恰好归还一次、
//! payload 的正文来源/预算身份可被发送 owner 读取。

use bytes::Bytes;
use std::sync::Arc;

use crate::config::Compression;
use crate::error::LangfuseError;
use crate::memory::MemoryBudget;
use crate::payload::{BudgetedBytes, ExportPayload, PayloadBody};

fn budget(limit: usize) -> Arc<MemoryBudget> {
    MemoryBudget::new(limit).expect("budget")
}

#[test]
fn new_rejects_permit_smaller_than_body() {
    let budget = budget(1024);
    let permit = budget.try_reserve(3).expect("permit");
    let error = match BudgetedBytes::new(Bytes::from_static(b"abcd"), permit) {
        Ok(_) => panic!("permit must cover the body"),
        Err(error) => error,
    };
    assert!(matches!(error, LangfuseError::BudgetMismatch));
}

#[test]
fn shared_body_keeps_permit_until_last_reference() {
    let budget = budget(1024);
    let permit = budget.try_reserve(16).expect("permit");
    let shared = BudgetedBytes::new(Bytes::from_static(b"0123456789abcd"), permit)
        .expect("bounded body")
        .into_shared();
    assert_eq!(shared.len(), 14);
    assert_eq!(budget.used(), 16);

    // 只克隆 `Bytes` 会提前归还 permit；共享必须整体 `Arc` 化。
    let second_owner = Arc::clone(&shared);
    drop(shared);
    assert_eq!(budget.used(), 16);
    assert_eq!(second_owner.bytes().as_ref(), b"0123456789abcd");
    drop(second_owner);
    assert_eq!(budget.used(), 0);
}

#[test]
fn payload_exposes_body_source_and_budget_identity() {
    let budget = budget(4096);
    let permit = budget.try_reserve(64).expect("permit");
    let body = BudgetedBytes::new(Bytes::from_static(b"{\"batch\":[]}"), permit).expect("body");
    let payload = ExportPayload {
        body: PayloadBody::Memory(body),
        encoding: Compression::Gzip,
        raw_len: 12,
        wire_len: 40,
        sha256: [7; 32],
        event_count: 1,
        budget: Arc::clone(&budget),
    };

    assert!(payload.is_memory());
    assert_eq!(payload.body_len(), 12);
    assert_eq!(payload.wire_len, 40);
    assert_eq!(payload.raw_len, 12);
    assert!(Arc::ptr_eq(payload.budget(), &budget));

    drop(payload);
    assert_eq!(budget.used(), 0);
}
