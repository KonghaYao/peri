//! 取消请求的作用域、在途去重与完成发布回归；复用父模块挂载及发送接缝。
use super::BRIDGE_RESET_COUNTER;
use super::BgTasksGuard;
use super::CancelGates;
use super::CancelGuard;
use super::CancelTarget;
use super::KeyCode;
use super::NotificationGuard;
use super::OperationTicket;
use super::PanelStackGuard;
use super::SessionGuard;
use super::TasksSourcesGuard;
use super::serial;
use peri_acp::transport::types::AcpError;
use ratatui_kit::prelude::*;

struct ResetGuard(u64);

impl Drop for ResetGuard {
    fn drop(&mut self) {
        BRIDGE_RESET_COUNTER.set(self.0);
    }
}

fn take_ticket() -> (OperationTicket, State<CancelGates>, CancelTarget) {
    CANCEL_TICKETS.with(|tickets| tickets.borrow_mut().remove(0))
}

#[test]
#[serial]
fn repeated_enter_sends_one_cancel_request() {
    let _sources = TasksSourcesGuard::empty();
    let _bg = BgTasksGuard::install(vec![super::bg_task("bg0")]);
    let _panels = PanelStackGuard::install();
    let _cancel = CancelGuard::install();
    let _session = SessionGuard::install(Some("s-1"));
    let _notice = NotificationGuard::install();
    super::mount_panel_frames(2, |frame, panel| {
        if frame != 1 {
            return;
        }
        panel.event(super::key(KeyCode::Enter));
        panel.event(super::key(KeyCode::Enter));
        assert_eq!(CancelGuard::requests().len(), 1);
        assert!(!PanelStackGuard::panel_is_open());
        assert!(NotificationGuard::message().is_none());
    });
}

#[test]
#[serial]
fn reset_before_completion_discards_cancel_notification() {
    let _sources = TasksSourcesGuard::empty();
    let _bg = BgTasksGuard::install(vec![super::bg_task("bg0")]);
    let _panels = PanelStackGuard::install();
    let _cancel = CancelGuard::install();
    let _session = SessionGuard::install(Some("s-1"));
    let _notice = NotificationGuard::install();
    let _reset = ResetGuard(BRIDGE_RESET_COUNTER.get());
    super::mount_panel_frames(2, |frame, panel| {
        if frame != 1 {
            return;
        }
        panel.event(super::key(KeyCode::Enter));
        let (ticket, gates, target) = take_ticket();
        BRIDGE_RESET_COUNTER.set(BRIDGE_RESET_COUNTER.get() + 1);
        assert!(!super::complete_cancel(
            gates,
            &ticket,
            &target,
            Err(AcpError::new(-32603, "late failure"))
        ));
        assert!(NotificationGuard::message().is_none());
        // 完成回调不得在拒收时顺手清理当前状态。
        assert!(gates.read()["bg0"].is_pending());
    });
}

#[test]
#[serial]
fn accepted_failure_notifies_once_and_cancelled_ticket_never_notifies() {
    let _sources = TasksSourcesGuard::empty();
    let _bg = BgTasksGuard::install(vec![super::bg_task("bg0")]);
    let _panels = PanelStackGuard::install();
    let _cancel = CancelGuard::install();
    let _session = SessionGuard::install(Some("s-1"));
    let _notice = NotificationGuard::install();
    super::mount_panel_frames(2, |frame, panel| {
        if frame != 1 {
            return;
        }
        panel.event(super::key(KeyCode::Enter));
        let (ticket, gates, target) = take_ticket();
        assert!(super::complete_cancel(
            gates,
            &ticket,
            &target,
            Err(AcpError::new(-32603, "failure"))
        ));
        let message = NotificationGuard::message().expect("accepted failure notifies once and cancelled ticket never notifies: the message fixture operation uses valid inputs");
        assert!(message.contains("failure"));
        assert!(!super::complete_cancel(
            gates,
            &ticket,
            &target,
            Ok(serde_json::json!({}))
        ));
        assert_eq!(
            NotificationGuard::message().as_deref(),
            Some(message.as_str())
        );
        assert!(super::dispatch_cancel(
            target.clone(),
            super::current_scope(),
            gates
        ));
        let (ticket, gates, target) = take_ticket();
        gates.write().get_mut("bg0").expect("accepted failure notifies once and cancelled ticket never notifies: fixture must contain the requested entry").cancel();
        assert!(!super::complete_cancel(
            gates,
            &ticket,
            &target,
            Err(AcpError::new(-32603, "cancelled"))
        ));
        assert_eq!(
            NotificationGuard::message().as_deref(),
            Some(message.as_str())
        );
    });
}

#[test]
#[serial]
fn scope_reset_effect_cancels_wait_on_next_frame() {
    let _sources = TasksSourcesGuard::empty();
    let _panels = PanelStackGuard::install();
    let _session = SessionGuard::install(Some("s-1"));
    let _reset = ResetGuard(BRIDGE_RESET_COUNTER.get());
    let mut ticket = None;
    super::mount_panel_frames(3, move |frame, panel| match frame {
        1 => {
            ticket = Some(
                panel
                    .cancel_gates
                    .write()
                    .entry("bg0".into())
                    .or_default()
                    .begin(super::current_scope()),
            );
            BRIDGE_RESET_COUNTER.set(BRIDGE_RESET_COUNTER.get() + 1);
        }
        2 => {
            assert!(ticket.as_ref().expect("scope reset effect cancels wait on next frame: the fixture must retain its expected variant").is_cancelled());
            assert!(panel.cancel_gates.read().is_empty());
        }
        _ => {}
    });
}

#[test]
#[serial]
fn unmounted_panel_rejects_result_without_notification() {
    let _sources = TasksSourcesGuard::empty();
    let _bg = BgTasksGuard::install(vec![super::bg_task("bg0")]);
    let _panels = PanelStackGuard::install();
    let _cancel = CancelGuard::install();
    let _session = SessionGuard::install(Some("s-1"));
    let _notice = NotificationGuard::install();
    super::mount_panel_frames(2, |frame, panel| {
        if frame == 1 {
            panel.event(super::key(KeyCode::Enter));
        }
    });
    let (ticket, gates, target) = take_ticket();
    assert!(ticket.is_cancelled());
    assert!(!super::complete_cancel(
        gates,
        &ticket,
        &target,
        Err(AcpError::new(-32603, "late failure"))
    ));
    assert!(NotificationGuard::message().is_none());
}

use super::CANCEL_TICKETS;
