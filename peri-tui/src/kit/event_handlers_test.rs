//! Tests

use std::time::{Duration, Instant};

use super::*;
use crate::kit::atoms::{ACTIVE_PANEL, OPEN_PANELS};

fn setup_atoms() {
    crate::kit::atoms::init_atoms();
    *OPEN_PANELS.state().write() = Vec::new();
    *ACTIVE_PANEL.state().write() = None;
}

#[test]
fn test_setup_atoms_initializes_empty() {
    setup_atoms();
    assert!(OPEN_PANELS.state().read().is_empty());
    assert!(ACTIVE_PANEL.state().read().is_none());
}

// ── determine_ctrl_c_action 测试 ──────────────────────────────────────

#[test]
fn test_determine_ctrl_c_action_loading() {
    // loading 状态下始终返回 Cancel
    let now = Instant::now();
    assert_eq!(
        determine_ctrl_c_action(true, None, now),
        CtrlCAction::Cancel
    );
    assert_eq!(
        determine_ctrl_c_action(true, Some(now), now),
        CtrlCAction::Cancel
    );
    assert_eq!(
        determine_ctrl_c_action(true, Some(now - Duration::from_millis(500)), now),
        CtrlCAction::Cancel
    );
}

#[test]
fn test_determine_ctrl_c_action_idle_first() {
    // 空闲状态下首次按 Ctrl+C 返回 FirstQuit
    let now = Instant::now();
    assert_eq!(
        determine_ctrl_c_action(false, None, now),
        CtrlCAction::FirstQuit
    );
}

#[test]
fn test_determine_ctrl_c_action_idle_double() {
    // 空闲状态下 1 秒内双击返回 Quit
    let now = Instant::now();
    let first = now - Duration::from_millis(500);
    assert_eq!(
        determine_ctrl_c_action(false, Some(first), now),
        CtrlCAction::Quit
    );
}

#[test]
fn test_determine_ctrl_c_action_idle_expired() {
    // 空闲状态下超过 1 秒未双击，重置为 FirstQuit
    let now = Instant::now();
    let first = now - Duration::from_millis(1500);
    assert_eq!(
        determine_ctrl_c_action(false, Some(first), now),
        CtrlCAction::FirstQuit
    );
}

/// Issue 2026-08-05 验收：transport 死亡且 notifier 兜底复位后
/// （is_loading=false），Ctrl+C 退出链路恢复可达——第一次 → FirstQuit，
/// 1 秒内第二次 → Quit，不再被 loading 门禁恒拦截为 Cancel。
#[test]
fn test_disconnected_recovery_makes_ctrl_c_quit_reachable() {
    let now = Instant::now();
    assert_eq!(
        determine_ctrl_c_action(false, None, now),
        CtrlCAction::FirstQuit
    );
    let first = now - Duration::from_millis(500);
    assert_eq!(
        determine_ctrl_c_action(false, Some(first), now),
        CtrlCAction::Quit
    );
}
