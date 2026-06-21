use std::thread;

use super::*;

#[test]
fn test_next_cycle() {
    assert_eq!(PermissionMode::Default.next(), PermissionMode::AcceptEdit);
    assert_eq!(PermissionMode::AcceptEdit.next(), PermissionMode::AutoMode);
    assert_eq!(PermissionMode::AutoMode.next(), PermissionMode::Bypass);
    assert_eq!(PermissionMode::Bypass.next(), PermissionMode::Default);
}

#[test]
fn test_default() {
    assert_eq!(PermissionMode::default(), PermissionMode::Default);
}

#[test]
fn test_display_name() {
    assert_eq!(PermissionMode::Default.display_name(), "");
    assert_eq!(PermissionMode::AcceptEdit.display_name(), "Accept Edit");
    assert_eq!(PermissionMode::AutoMode.display_name(), "Auto Mode");
    assert_eq!(PermissionMode::Bypass.display_name(), "Bypass");
}

#[test]
fn test_from_u8_valid() {
    assert_eq!(PermissionMode::from(0u8), PermissionMode::Default);
    assert_eq!(PermissionMode::from(2u8), PermissionMode::AcceptEdit);
    assert_eq!(PermissionMode::from(3u8), PermissionMode::AutoMode);
    assert_eq!(PermissionMode::from(4u8), PermissionMode::Bypass);
}

#[test]
fn test_from_u8_invalid() {
    // 值 1 为已删除的 DontAsk 历史占位，现在映射到 Default
    assert_eq!(PermissionMode::from(1u8), PermissionMode::Default);
    assert_eq!(PermissionMode::from(5u8), PermissionMode::Default);
    assert_eq!(PermissionMode::from(6u8), PermissionMode::Default);
    assert_eq!(PermissionMode::from(255u8), PermissionMode::Default);
}

#[test]
fn test_shared_new_and_load() {
    let shared = SharedPermissionMode::new(PermissionMode::AutoMode);
    assert_eq!(shared.load(), PermissionMode::AutoMode);
}

#[test]
fn test_shared_store_and_load() {
    let shared = SharedPermissionMode::new(PermissionMode::Default);
    shared.store(PermissionMode::Bypass);
    assert_eq!(shared.load(), PermissionMode::Bypass);
}

#[test]
fn test_shared_cycle_single_thread() {
    let shared = SharedPermissionMode::new(PermissionMode::Default);
    assert_eq!(shared.cycle(), PermissionMode::AcceptEdit);
    assert_eq!(shared.cycle(), PermissionMode::AutoMode);
    assert_eq!(shared.cycle(), PermissionMode::Bypass);
    assert_eq!(shared.cycle(), PermissionMode::Default);
}

#[test]
fn test_shared_cycle_concurrent() {
    let shared = SharedPermissionMode::new(PermissionMode::Default);
    let shared_clone = shared.clone();
    let barrier = Arc::new(std::sync::Barrier::new(4));

    let mut handles = vec![];
    for _ in 0..4 {
        let s = shared_clone.clone();
        let b = barrier.clone();
        handles.push(thread::spawn(move || {
            b.wait();
            for _ in 0..100 {
                s.cycle();
            }
        }));
    }

    for h in handles {
        h.join().unwrap();
    }

    // 最终状态应为合法 PermissionMode
    let final_mode = shared.load();
    assert!(matches!(
        final_mode,
        PermissionMode::Default
            | PermissionMode::AcceptEdit
            | PermissionMode::AutoMode
            | PermissionMode::Bypass
    ));
}
