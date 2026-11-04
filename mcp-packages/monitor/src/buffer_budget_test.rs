use super::*;
use crate::buffer::{MAX_READ_BYTES, MAX_TASK_BYTES, MIN_TASK_BYTES};

fn captured(text: String) -> CapturedEvent {
    CapturedEvent {
        ts: "2026-11-04T00:00:00Z".into(),
        stream: "stdout",
        kind: "line",
        bytes: text.len() as u64,
        text,
        truncated: false,
        partial: false,
    }
}

#[test]
fn shared_budget_evicts_events_across_tasks_without_resetting_cursors() {
    let budget = BufferBudget::new(64 * 1024);
    let first = Arc::new(Mutex::new(EventBuffer::new(100_000, MAX_TASK_BYTES)));
    let second = Arc::new(Mutex::new(EventBuffer::new(100_000, MAX_TASK_BYTES)));
    budget.register("first".into(), first.clone());
    budget.register("second".into(), second.clone());
    for index in 0..100 {
        let id = if index % 2 == 0 { "first" } else { "second" };
        assert!(budget.push(id, captured("x".repeat(8192))).is_some());
        assert!(budget.resident_bytes() <= 64 * 1024);
    }
    assert_eq!(first.lock().last_assigned_seq(), 50);
    assert_eq!(second.lock().last_assigned_seq(), 50);
    let total = first.lock().resident_bytes() + second.lock().resident_bytes();
    assert_eq!(total, budget.resident_bytes());
    assert!(first.lock().dropped() + second.lock().dropped() > 0);
    let page = second.lock().read(None, 500, MAX_READ_BYTES);
    assert!(page.events.windows(2).all(|pair| pair[0].seq < pair[1].seq));
    assert!(page.dropped > 0);
}

#[test]
fn metadata_and_spare_string_capacity_cannot_bypass_budget() {
    let budget = BufferBudget::new(16 * 1024);
    let buffer = Arc::new(Mutex::new(EventBuffer::new(100_000, MIN_TASK_BYTES)));
    budget.register("task".into(), buffer.clone());
    for _ in 0..1000 {
        assert!(budget.push("task", captured(String::new())).is_some());
    }
    assert!(
        buffer.lock().dropped() > 0,
        "empty events still allocate metadata"
    );
    assert!(budget.resident_bytes() <= 16 * 1024);
    let mut oversized = String::with_capacity(1024 * 1024);
    oversized.push('x');
    assert!(budget.push("task", captured(oversized)).is_some());
    assert!(budget.resident_bytes() <= 16 * 1024);
    assert_eq!(buffer.lock().resident_bytes(), budget.resident_bytes());
}

#[test]
fn removal_reclaims_backing_storage_even_when_reader_holds_the_buffer() {
    let budget = BufferBudget::new(64 * 1024);
    let buffer = Arc::new(Mutex::new(EventBuffer::new(1000, MAX_TASK_BYTES)));
    budget.register("task".into(), buffer.clone());
    for _ in 0..100 {
        assert!(budget.push("task", captured("retained".into())).is_some());
    }
    assert!(buffer.lock().resident_bytes() > 0);
    budget.remove("task");
    assert_eq!(budget.resident_bytes(), 0);
    assert_eq!(buffer.lock().resident_bytes(), 0);
    assert_eq!(buffer.lock().last_assigned_seq(), 100);
    assert_eq!(buffer.lock().dropped(), 100);
    assert!(budget.push("task", captured("late".into())).is_none());
}

#[test]
fn concurrent_writers_share_one_bounded_account() {
    let budget = Arc::new(BufferBudget::new(64 * 1024));
    let mut writers = Vec::new();
    for index in 0..4 {
        let id = format!("task-{index}");
        budget.register(
            id.clone(),
            Arc::new(Mutex::new(EventBuffer::new(100_000, MAX_TASK_BYTES))),
        );
        let budget = budget.clone();
        writers.push(std::thread::spawn(move || {
            for _ in 0..200 {
                assert!(budget.push(&id, captured("x".repeat(8192))).is_some());
                assert!(budget.resident_bytes() <= 64 * 1024);
            }
        }));
    }
    for writer in writers {
        writer
            .join()
            .expect("writer finishes without lock inversion");
    }
}

#[test]
fn two_64_mib_producers_cannot_accumulate_128_mib_of_retained_events() {
    let budget = BufferBudget::new(INSTANCE_EVENT_BYTES);
    let mut buffers = Vec::new();
    for index in 0..2 {
        let id = format!("producer-{index}");
        let buffer = Arc::new(Mutex::new(EventBuffer::new(100_000, MAX_TASK_BYTES)));
        budget.register(id.clone(), buffer.clone());
        for _ in 0..8192 {
            assert!(budget.push(&id, captured("x".repeat(8192))).is_some());
        }
        assert_eq!(buffer.lock().last_assigned_seq(), 8192);
        assert!(budget.resident_bytes() <= INSTANCE_EVENT_BYTES);
        buffers.push(buffer);
    }
    assert!(buffers.iter().any(|buffer| buffer.lock().dropped() > 0));
    assert_eq!(
        budget.resident_bytes(),
        buffers
            .iter()
            .map(|buffer| buffer.lock().resident_bytes())
            .sum::<usize>()
    );
}
