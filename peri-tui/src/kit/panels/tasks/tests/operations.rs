//! 取消请求的作用域、在途去重与完成发布回归；复用父模块挂载及发送接缝。
use super::*;
use peri_acp::transport::types::AcpError;

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
    let _bg = BgTasksGuard::install(vec![bg_task("bg0")]);
    let _panels = PanelStackGuard::install();
    let _cancel = CancelGuard::install();
    let _session = SessionGuard::install(Some("s-1"));
    let _notice = NotificationGuard::install();
    mount_panel_frames(2, |frame, panel| {
        if frame != 1 {
            return;
        }
        panel.event(key(KeyCode::Enter));
        panel.event(key(KeyCode::Enter));
        assert_eq!(CancelGuard::requests().len(), 1);
        assert!(!PanelStackGuard::panel_is_open());
        assert!(NotificationGuard::message().is_none());
    });
}

#[test]
#[serial]
fn reset_before_completion_discards_cancel_notification() {
    let _sources = TasksSourcesGuard::empty();
    let _bg = BgTasksGuard::install(vec![bg_task("bg0")]);
    let _panels = PanelStackGuard::install();
    let _cancel = CancelGuard::install();
    let _session = SessionGuard::install(Some("s-1"));
    let _notice = NotificationGuard::install();
    let _reset = ResetGuard(BRIDGE_RESET_COUNTER.get());
    mount_panel_frames(2, |frame, panel| {
        if frame != 1 {
            return;
        }
        panel.event(key(KeyCode::Enter));
        let (ticket, gates, target) = take_ticket();
        BRIDGE_RESET_COUNTER.set(BRIDGE_RESET_COUNTER.get() + 1);
        assert!(!complete_cancel(
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
    let _bg = BgTasksGuard::install(vec![bg_task("bg0")]);
    let _panels = PanelStackGuard::install();
    let _cancel = CancelGuard::install();
    let _session = SessionGuard::install(Some("s-1"));
    let _notice = NotificationGuard::install();
    mount_panel_frames(2, |frame, panel| {
        if frame != 1 {
            return;
        }
        panel.event(key(KeyCode::Enter));
        let (ticket, gates, target) = take_ticket();
        assert!(complete_cancel(
            gates,
            &ticket,
            &target,
            Err(AcpError::new(-32603, "failure"))
        ));
        let message = NotificationGuard::message().unwrap();
        assert!(message.contains("failure"));
        assert!(!complete_cancel(
            gates,
            &ticket,
            &target,
            Ok(serde_json::json!({}))
        ));
        assert_eq!(
            NotificationGuard::message().as_deref(),
            Some(message.as_str())
        );
        assert!(dispatch_cancel(target.clone(), current_scope(), gates));
        let (ticket, gates, target) = take_ticket();
        gates.write().get_mut("bg0").unwrap().cancel();
        assert!(!complete_cancel(
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
    mount_panel_frames(3, move |frame, panel| match frame {
        1 => {
            ticket = Some(
                panel
                    .cancel_gates
                    .write()
                    .entry("bg0".into())
                    .or_default()
                    .begin(current_scope()),
            );
            BRIDGE_RESET_COUNTER.set(BRIDGE_RESET_COUNTER.get() + 1);
        }
        2 => {
            assert!(ticket.as_ref().unwrap().is_cancelled());
            assert!(panel.cancel_gates.read().is_empty());
        }
        _ => {}
    });
}

#[test]
#[serial]
fn unmounted_panel_rejects_result_without_notification() {
    let _sources = TasksSourcesGuard::empty();
    let _bg = BgTasksGuard::install(vec![bg_task("bg0")]);
    let _panels = PanelStackGuard::install();
    let _cancel = CancelGuard::install();
    let _session = SessionGuard::install(Some("s-1"));
    let _notice = NotificationGuard::install();
    mount_panel_frames(2, |frame, panel| {
        if frame == 1 {
            panel.event(key(KeyCode::Enter));
        }
    });
    let (ticket, gates, target) = take_ticket();
    assert!(ticket.is_cancelled());
    assert!(!complete_cancel(
        gates,
        &ticket,
        &target,
        Err(AcpError::new(-32603, "late failure"))
    ));
    assert!(NotificationGuard::message().is_none());
}
