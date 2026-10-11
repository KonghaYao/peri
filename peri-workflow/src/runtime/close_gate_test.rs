//! `close_gate.rs` 的门语义测试：准入可并发、关闭等待在途准入、关闭后拒绝准入。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use super::CloseGate;

#[test]
fn admits_are_shared_until_closed() {
    let gate = CloseGate::new();
    assert!(!gate.is_closed());
    let first = gate.admit().expect("开启状态放行准入");
    let second = gate.admit().expect("准入可并发持票");
    drop((first, second));

    gate.mark_closed();
    assert!(gate.is_closed());
    assert!(gate.admit().is_none(), "关闭后不再放行准入");
}

#[test]
fn mark_closed_waits_for_in_flight_ticket() {
    let gate = Arc::new(CloseGate::new());
    let ticket = gate.admit().expect("开启状态放行准入");

    let started = Arc::new(AtomicBool::new(false));
    let closed = Arc::new(AtomicBool::new(false));
    let closer = {
        let gate = Arc::clone(&gate);
        let started = Arc::clone(&started);
        let closed = Arc::clone(&closed);
        std::thread::spawn(move || {
            started.store(true, Ordering::SeqCst);
            gate.mark_closed();
            closed.store(true, Ordering::SeqCst);
        })
    };

    while !started.load(Ordering::SeqCst) {
        std::thread::yield_now();
    }
    std::thread::sleep(Duration::from_millis(50));
    assert!(
        !closed.load(Ordering::SeqCst),
        "在途准入未结束前 mark_closed 必须等待"
    );

    drop(ticket);
    closer.join().expect("关闭线程正常结束");
    assert!(closed.load(Ordering::SeqCst), "票据释放后关闭完成");
    assert!(gate.is_closed());
    assert!(gate.admit().is_none(), "关闭返回后准入一律拒绝");
}
