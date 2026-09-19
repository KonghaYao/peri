use super::*;
use crate::kit::atoms::{CONFIRM_PAYLOAD, POPUP_KIND, PopupKind};
use peri_acp_types::workspace::RecoveryRequiredDetails;
use ratatui_kit::ratatui::layout::Rect;
use serial_test::serial;
use std::time::Duration;

fn target() -> RecoveryRequiredDetails {
    RecoveryRequiredDetails {
        thread_id: "thread-a".to_string(),
        generation: 3,
    }
}

/// 本文件只写 popup 两个 atom；仍按 RAII 保存/恢复，避免与并行 lib 测试互相污染。
struct PopupAtomsGuard {
    popup: Option<PopupKind>,
    payload: Option<atoms::ConfirmPayload>,
}

impl PopupAtomsGuard {
    fn capture() -> Self {
        let guard = Self {
            popup: *POPUP_KIND.state().read(),
            payload: CONFIRM_PAYLOAD.state().read().clone(),
        };
        *POPUP_KIND.state().write() = None;
        *CONFIRM_PAYLOAD.state().write() = None;
        guard
    }
}

impl Drop for PopupAtomsGuard {
    fn drop(&mut self) {
        *POPUP_KIND.state().write() = self.popup;
        *CONFIRM_PAYLOAD.state().write() = self.payload.clone();
    }
}

fn owner(
    displayed: bool,
) -> (
    std::sync::Arc<RecoveryConfirmation>,
    tokio::sync::oneshot::Receiver<bool>,
) {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let owner = std::sync::Arc::new(RecoveryConfirmation {
        target: target(),
        response: std::sync::Mutex::new(Some(tx)),
        displayed: std::sync::atomic::AtomicBool::new(displayed),
    });
    (owner, rx)
}

async fn wait_for_payload() {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if CONFIRM_PAYLOAD.state().read().is_some() {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("recovery confirmation must publish its payload");
}

/// 通用确认路径（Enter/点击整窗）绝不能代替专用风险选择。
#[tokio::test]
#[serial]
async fn test_dirty_recovery_generic_confirm_path_cannot_accept_risk() {
    let _guard = PopupAtomsGuard::capture();
    let (tx, rx) = tokio::sync::oneshot::channel();
    let confirmation = std::sync::Arc::new(RecoveryConfirmation {
        target: target(),
        response: std::sync::Mutex::new(Some(tx)),
        displayed: std::sync::atomic::AtomicBool::new(true),
    });
    execute_confirm_action(&ConfirmAction::RecoverDirty(confirmation), |_| {});
    assert!(!rx.await.unwrap(), "通用确认路径必须按取消收敛");
}

/// 没有经过渲染确认可见时，任何 accept 都必须失败闭合。
#[tokio::test]
#[serial]
async fn test_dirty_recovery_accept_requires_displayed_confirmation() {
    let _guard = PopupAtomsGuard::capture();
    let (hidden, hidden_rx) = owner(false);
    {
        let mut tracker = RecoveryDisplay {
            owner: hidden,
            width: 60,
            height: 11,
            area: None,
        };
        // 终端比确认内容更小：不登记矩形，并立即按取消收敛。
        tracker.record_area(Rect::new(0, 0, 20, 4));
        assert!(tracker.area.is_none());
    }
    assert!(!hidden_rx.await.unwrap(), "未渲染确认必须按取消收敛");

    let (visible, visible_rx) = owner(false);
    let mut tracker = RecoveryDisplay {
        owner: visible.clone(),
        width: 60,
        height: 11,
        area: None,
    };
    tracker.record_area(Rect::new(4, 2, 60, 11));
    assert_eq!(tracker.area, Some(Rect::new(4, 2, 60, 11)));
    visible.answer(true);
    assert!(visible_rx.await.unwrap(), "已渲染且接受风险时才能确认");
}

/// 默认选中取消：Enter（未切换）与 Esc 都是取消，选择后才可确认接受。
#[test]
#[serial]
fn test_dirty_recovery_default_selection_is_cancel() {
    let mut selected = false;
    assert_eq!(
        recovery_choice(
            &Event::Key(ratatui_kit::crossterm::event::KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE
            )),
            &mut selected,
            None
        ),
        Some(false)
    );
    assert_eq!(
        recovery_choice(
            &Event::Key(ratatui_kit::crossterm::event::KeyEvent::new(
                KeyCode::Esc,
                KeyModifiers::NONE
            )),
            &mut selected,
            None
        ),
        Some(false)
    );
    assert_eq!(
        recovery_choice(
            &Event::Key(ratatui_kit::crossterm::event::KeyEvent::new(
                KeyCode::Down,
                KeyModifiers::NONE
            )),
            &mut selected,
            None
        ),
        None
    );
    assert!(selected);
    assert_eq!(
        recovery_choice(
            &Event::Key(ratatui_kit::crossterm::event::KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE
            )),
            &mut selected,
            None
        ),
        Some(true)
    );
    // 输入与其他快捷键不产生选择，也不能落进背景输入区。
    assert_eq!(
        recovery_choice(
            &Event::Key(ratatui_kit::crossterm::event::KeyEvent::new(
                KeyCode::Char('y'),
                KeyModifiers::NONE
            )),
            &mut selected,
            None
        ),
        None
    );
    assert_eq!(
        recovery_choice(
            &Event::Key(ratatui_kit::crossterm::event::KeyEvent::new(
                KeyCode::Char('c'),
                KeyModifiers::CONTROL
            )),
            &mut selected,
            None
        ),
        None
    );
}

/// 鼠标只在确认内容区内、且命中明确行时产生选择。
#[test]
#[serial]
fn test_dirty_recovery_mouse_requires_recorded_area_rows() {
    let area = Some(Rect::new(4, 2, 60, 11));
    let click = |row: u16| {
        Event::Mouse(ratatui_kit::crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 10,
            row,
            modifiers: KeyModifiers::NONE,
        })
    };
    let mut selected = false;
    assert_eq!(
        recovery_choice(&click(10), &mut selected, area),
        Some(false)
    );
    assert_eq!(recovery_choice(&click(11), &mut selected, area), Some(true));
    assert_eq!(recovery_choice(&click(6), &mut selected, area), None);
    // 未渲染/未登记区域：整窗点击不产生任何选择。
    assert_eq!(recovery_choice(&click(11), &mut selected, None), None);
    assert_eq!(recovery_choice(&click(0), &mut selected, area), None);
}

/// 已有其他弹窗时不能抢占，也不能破坏原 payload。
#[tokio::test]
#[serial]
async fn test_dirty_recovery_fails_closed_when_popup_is_unavailable() {
    let _guard = PopupAtomsGuard::capture();
    *POPUP_KIND.state().write() = Some(PopupKind::Hitl);
    *CONFIRM_PAYLOAD.state().write() = Some(atoms::ConfirmPayload {
        title: "existing".into(),
        message: "existing".into(),
        details: vec![],
        pending_action: atoms::ConfirmAction::ThreadSwitch("other".into()),
    });
    assert!(!confirm_dirty_recovery(target()).await);
    assert_eq!(*POPUP_KIND.state().read(), Some(PopupKind::Hitl));
    assert_eq!(
        CONFIRM_PAYLOAD.state().read().as_ref().unwrap().title,
        "existing"
    );
}

/// 首帧渲染之前弹窗被 `open_popup` 覆盖：旧确认必须精确结清，新 popup 保留。
#[tokio::test]
#[serial]
async fn test_dirty_recovery_revoked_before_first_frame_answers_cancel() {
    let _guard = PopupAtomsGuard::capture();
    let waiter = tokio::spawn(confirm_dirty_recovery(target()));
    wait_for_payload().await;
    let before = *POPUP_KIND.state().read();

    // 尚未经过任何渲染帧（RecoveryDisplay 未建立，没有 Drop 兜底）。
    crate::kit::popup_overlay::open_popup(crate::kit::atoms::PopupKind::OAuth);
    assert_eq!(
        *POPUP_KIND.state().read(),
        Some(crate::kit::atoms::PopupKind::OAuth)
    );
    assert_eq!(before, Some(PopupKind::Confirm));
    assert!(CONFIRM_PAYLOAD.state().read().is_none());
    let answered = tokio::time::timeout(Duration::from_secs(5), waiter)
        .await
        .expect("revoked confirmation must not keep the waiter alive")
        .unwrap();
    assert!(!answered, "被覆盖的确认只能按取消收敛");

    // 撤销边界之后 close：新 popup 正常关闭，无残留 dirty payload。
    crate::kit::popup_overlay::close_popup();
    assert_eq!(*POPUP_KIND.state().read(), None);
    assert!(CONFIRM_PAYLOAD.state().read().is_none());
}

/// 等待期间会话切换/取消（future 被丢弃）必须按取消收敛并清理弹窗。
#[tokio::test]
#[serial]
async fn test_dirty_recovery_dropped_waiter_cancels_and_clears_popup() {
    let _guard = PopupAtomsGuard::capture();
    let waiter = tokio::spawn(confirm_dirty_recovery(target()));
    wait_for_payload().await;
    assert_eq!(*POPUP_KIND.state().read(), Some(PopupKind::Confirm));
    waiter.abort();
    let _ = waiter.await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if CONFIRM_PAYLOAD.state().read().is_none() && POPUP_KIND.state().read().is_none() {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("dropped recovery waiter must clear its popup");
}
