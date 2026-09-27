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

/// dirty 风险选择的说明正文：选项行是最后两行，行号不写死。
fn dirty_prompt() -> RiskPrompt {
    RiskPrompt::DirtyRecovery(target())
}

/// dirty 弹窗的「取消 / 接受」两行行号。
fn dirty_options() -> (u16, u16) {
    dirty_prompt().option_rows()
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
    std::sync::Arc<RiskConfirmation>,
    tokio::sync::oneshot::Receiver<RiskChoice>,
) {
    let (owner, rx) = RiskConfirmation::new(dirty_prompt());
    if displayed {
        owner.mark_displayed();
    }
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
    let (confirmation, rx) = owner(true);
    execute_confirm_action(&ConfirmAction::RiskChoice(confirmation), |_| {});
    assert_eq!(
        rx.await.unwrap(),
        RiskChoice::Declined,
        "通用确认路径必须按取消收敛"
    );
}

/// 没有经过渲染确认可见时，任何 accept 都必须失败闭合。
///
/// 装不下的帧**不是**用户的决定：这里不结清这次选择（旧行为会立刻按取消收敛，让
/// 「确认装不下」直接变成「用户没被问过、什么都没发生」——用户既没有机会，也看不到
/// 原因）。窗口够大之后同一次确认仍然成立，接受依旧要求已完整渲染。
#[tokio::test]
#[serial]
async fn test_dirty_recovery_accept_requires_displayed_confirmation() {
    let _guard = PopupAtomsGuard::capture();
    let (hidden, mut hidden_rx) = owner(false);
    let mut tracker = RiskDisplay {
        owner: hidden.clone(),
        width: 60,
        height: dirty_prompt().popup_height(),
        area: None,
    };
    // 终端比确认内容更小：不登记矩形，也不作答。
    tracker.record_area(Rect::new(0, 0, 20, 4));
    assert!(tracker.area.is_none());
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut hidden_rx)
            .await
            .is_err(),
        "装不下的帧不得替用户结清这次选择"
    );
    // 窗口变大后同一帧循环把矩形登记回来，这次确认仍然可用。
    tracker.record_area(Rect::new(4, 2, 60, 11));
    assert_eq!(tracker.area, Some(Rect::new(4, 2, 60, 11)));
    hidden.answer(false);
    assert_eq!(hidden_rx.await.unwrap(), RiskChoice::Declined);

    // 从未完整渲染就接受：不得算接受，也不得被讲成用户拒绝。
    let (not_shown, not_shown_rx) = owner(false);
    not_shown.answer(true);
    assert_eq!(not_shown_rx.await.unwrap(), RiskChoice::NotShown);

    let (visible, visible_rx) = owner(false);
    let mut tracker = RiskDisplay {
        owner: visible.clone(),
        width: 60,
        height: dirty_prompt().popup_height(),
        area: None,
    };
    tracker.record_area(Rect::new(4, 2, 60, 11));
    assert_eq!(tracker.area, Some(Rect::new(4, 2, 60, 11)));
    visible.answer(true);
    assert_eq!(
        visible_rx.await.unwrap(),
        RiskChoice::Accepted,
        "已渲染且接受风险时才能确认"
    );
}

/// 默认选中取消：Enter（未切换）与 Esc 都是取消，选择后才可确认接受。
#[test]
#[serial]
fn test_dirty_recovery_default_selection_is_cancel() {
    let mut selected = false;
    assert_eq!(
        risk_choice(
            &Event::Key(ratatui_kit::crossterm::event::KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE
            )),
            &mut selected,
            None,
            dirty_options()
        ),
        Some(false)
    );
    assert_eq!(
        risk_choice(
            &Event::Key(ratatui_kit::crossterm::event::KeyEvent::new(
                KeyCode::Esc,
                KeyModifiers::NONE
            )),
            &mut selected,
            None,
            dirty_options()
        ),
        Some(false)
    );
    assert_eq!(
        risk_choice(
            &Event::Key(ratatui_kit::crossterm::event::KeyEvent::new(
                KeyCode::Down,
                KeyModifiers::NONE
            )),
            &mut selected,
            None,
            dirty_options()
        ),
        None
    );
    assert!(selected);
    assert_eq!(
        risk_choice(
            &Event::Key(ratatui_kit::crossterm::event::KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE
            )),
            &mut selected,
            None,
            dirty_options()
        ),
        Some(true)
    );
    // 输入与其他快捷键不产生选择，也不能落进背景输入区。
    assert_eq!(
        risk_choice(
            &Event::Key(ratatui_kit::crossterm::event::KeyEvent::new(
                KeyCode::Char('y'),
                KeyModifiers::NONE
            )),
            &mut selected,
            None,
            dirty_options()
        ),
        None
    );
    assert_eq!(
        risk_choice(
            &Event::Key(ratatui_kit::crossterm::event::KeyEvent::new(
                KeyCode::Char('c'),
                KeyModifiers::CONTROL
            )),
            &mut selected,
            None,
            dirty_options()
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
        risk_choice(&click(10), &mut selected, area, dirty_options()),
        Some(false)
    );
    assert_eq!(
        risk_choice(&click(11), &mut selected, area, dirty_options()),
        Some(true)
    );
    assert_eq!(
        risk_choice(&click(6), &mut selected, area, dirty_options()),
        None
    );
    // 未渲染/未登记区域：整窗点击不产生任何选择。
    assert_eq!(
        risk_choice(&click(11), &mut selected, None, dirty_options()),
        None
    );
    assert_eq!(
        risk_choice(&click(0), &mut selected, area, dirty_options()),
        None
    );
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
    assert_eq!(
        confirm_risk_choice(dirty_prompt()).await,
        RiskChoice::NotShown,
        "位置被占用时这次询问没有发生，不能当成用户拒绝"
    );
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
    let waiter = tokio::spawn(confirm_risk_choice(dirty_prompt()));
    wait_for_payload().await;
    let before = *POPUP_KIND.state().read();

    // 尚未经过任何渲染帧（RiskDisplay 未建立，没有 Drop 兜底）。
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
    assert_eq!(
        answered,
        RiskChoice::NotShown,
        "被覆盖的确认从未呈现给用户，只能按未呈现收敛"
    );

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
    let waiter = tokio::spawn(confirm_risk_choice(dirty_prompt()));
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

/// dirty 说明的文案完整且已本地化：i18n 缺 key 时 `tr` 会回退成 key 文本，必须挡住。
#[test]
#[serial]
fn test_dirty_recovery_disclosure_is_localized_and_complete() {
    let (body, accept_key) = dirty_prompt().body();
    assert_eq!(accept_key, "dirty-recovery-accept");
    assert_eq!(
        body.len(),
        7,
        "dirty 说明七行：标题/风险/未知/责任/会话/代际/提示"
    );
    for line in &body {
        assert!(!line.is_empty(), "dirty 说明不得有空行");
        assert!(
            !line.starts_with("dirty-recovery-") && !line.starts_with("risk-choice-"),
            "缺少本地化文案: {line}"
        );
    }

    let lines = dirty_prompt().lines(false);
    assert_eq!(lines.len(), dirty_prompt().line_count());
    assert!(lines[7].starts_with('>'), "默认选中取消");
    assert!(lines[8].starts_with(' '), "默认不选中接受");
    let lines = dirty_prompt().lines(true);
    assert!(lines[8].starts_with('>'), "切换后选中接受");
    assert!(lines[7].starts_with(' '));
}
