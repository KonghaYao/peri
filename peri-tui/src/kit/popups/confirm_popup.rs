//! ratatui-kit ConfirmPopup component.
//!
//! 确认弹窗：从 `CONFIRM_PAYLOAD` atom 读取确认信息（title / message / details / pending_action），
//! Enter 执行确认，Esc 取消关闭。
//!
//! 同一文件另含风险选择专用确认（[`RiskPopup`]）：显式风险接受（解除 dirty 代际）
//! 不能复用「Enter 即确认」的通用语义，必须显式选择接受、默认取消，且确认内容未完整
//! 渲染时按取消收敛。

use ratatui_kit::{
    crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind},
    prelude::*,
    ratatui::{layout::Constraint, style::Stylize, text::Line},
};

use crate::i18n;
use crate::kit::ask_user_action::AskUserResponseAction;
use crate::kit::atoms::{self, ASK_USER_RESPONSE_TX, CONFIRM_PAYLOAD, ConfirmAction, LANG_VERSION};
use crate::kit::panel_mouse::AreaTracker;
use crate::kit::popup_overlay::close_popup;
use peri_theme::atoms::THEME_ATOM;

/// 一次显式风险选择要展示的领域载荷。
///
/// 只决定弹窗文案，不参与选择语义：选择语义（默认取消、一次性回答、未完整渲染即
/// 取消）对所有风险接受完全相同，因此共用一套实现。新增一种风险接受在这里加一个
/// 变体，不要另建一套弹窗。
/// 提示：`Debug` 只打印风险种类，不打印等待通道。
#[derive(Debug)]
pub(crate) enum RiskPrompt {
    /// 解除某条精确的 dirty 执行代际（`peri/session_reset_dirty`）。
    DirtyRecovery(peri_acp_types::workspace::RecoveryRequiredDetails),
}

impl RiskPrompt {
    /// 弹窗说明正文（不含选项行）与接受项文案 key。
    ///
    /// 每次调用现取 i18n 文本：语言在弹窗展示期间切换时重渲染要跟着变。
    fn body(&self) -> (Vec<String>, &'static str) {
        match self {
            Self::DirtyRecovery(target) => (
                vec![
                    i18n::tr("dirty-recovery-title"),
                    i18n::tr("dirty-recovery-risk"),
                    i18n::tr("dirty-recovery-unknown"),
                    i18n::tr("dirty-recovery-responsibility"),
                    target.thread_id.clone(),
                    format!("generation {}", target.generation),
                    i18n::tr("dirty-recovery-hint"),
                ],
                "dirty-recovery-accept",
            ),
        }
    }

    /// 完整弹窗正文：说明正文 + 「取消 / 接受」两行，选中项加 `>` 前缀。
    fn lines(&self, accept_selected: bool) -> Vec<String> {
        let (mut lines, accept_key) = self.body();
        lines.push(Self::option(!accept_selected, "risk-choice-cancel"));
        lines.push(Self::option(accept_selected, accept_key));
        lines
    }

    /// 弹窗内容行数（含选项行）：渲染高度与可见性判定都用它。
    fn line_count(&self) -> usize {
        self.body().0.len() + 2
    }

    /// 弹窗需要的高度：内容行 + 上下边框。
    ///
    /// 可见性判定用（内容没被完整画出来就不能算「用户看到了风险说明」）。
    fn popup_height(&self) -> u16 {
        (self.line_count() + 2).min(u16::MAX as usize) as u16
    }

    /// 选项行文案：未选中加空格占位，选中加 `>` 前缀（宽度不随选择变化）。
    fn option(selected: bool, key: &str) -> String {
        format!("{} {}", if selected { ">" } else { " " }, i18n::tr(key))
    }

    /// 「取消 / 接受」两行相对弹窗区域顶部的行号（0-based，含上边框）。
    ///
    /// 鼠标命中判定用：正文行数随载荷变化，选项行必须跟着算，不能写死。
    fn option_rows(&self) -> (u16, u16) {
        let lines = self.line_count() as u16;
        (lines - 1, lines)
    }
}

/// 一次显式风险选择的结论。
///
/// 调用方必须能区分三者：把它们压成一个 `bool` 会让「用户拒绝」与「用户根本没拿到
/// 确认机会」无法分辨，而后者是要报告给用户的失败，不是用户的选择。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RiskChoice {
    /// 确认内容已完整渲染且用户显式接受——唯一允许动作的结论。
    Accepted,
    /// 用户看到完整确认后没有接受（Esc / 默认项 / 被替换）。
    Declined,
    /// 确认内容从未完整渲染给用户（首帧之前被抢占、窗口装不下、等待方被丢弃）：
    /// 这不是用户的决定，调用方不得把它讲成「用户拒绝」。
    NotShown,
}

/// 一次性选择与显示许可；不持有 client，不能重新选择当前会话。
#[derive(Debug)]
pub struct RiskConfirmation {
    pub(crate) prompt: RiskPrompt,
    response: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<RiskChoice>>>,
    displayed: std::sync::atomic::AtomicBool,
}

impl RiskConfirmation {
    /// 建立一次风险选择：返回所有者与等待方。
    ///
    /// 所有者由弹窗路径持有；等待方在所有者被替换、撤销、被丢弃或确认内容未完整
    /// 渲染时按取消收敛，因此调用方不需要超时兜底。
    pub(crate) fn new(
        prompt: RiskPrompt,
    ) -> (
        std::sync::Arc<Self>,
        tokio::sync::oneshot::Receiver<RiskChoice>,
    ) {
        let (tx, rx) = tokio::sync::oneshot::channel();
        (
            std::sync::Arc::new(Self {
                prompt,
                response: std::sync::Mutex::new(Some(tx)),
                displayed: std::sync::atomic::AtomicBool::new(false),
            }),
            rx,
        )
    }

    /// 结清这次选择。
    ///
    /// `accepted` 只是「用户按了接受」这一动作；能不能算接受由**查看时的可见性**
    /// 决定：确认内容没完整渲染出来时，接受一律降级为 [`RiskChoice::NotShown`]。
    /// 可见性每次查看现取，不缓存——缓存会让「先渲染过、后来窗口变小」的接受蒙混过关。
    pub(crate) fn answer(&self, accepted: bool) {
        if let Some(tx) = self.response.lock().unwrap().take() {
            let visible = self.displayed.load(std::sync::atomic::Ordering::Acquire);
            let choice = match (accepted, visible) {
                (true, true) => RiskChoice::Accepted,
                (false, true) => RiskChoice::Declined,
                (_, false) => RiskChoice::NotShown,
            };
            let _ = tx.send(choice);
        }
    }

    /// 测试替身：模拟确认内容已在终端完整渲染。
    #[cfg(test)]
    pub(crate) fn mark_displayed(&self) {
        self.displayed
            .store(true, std::sync::atomic::Ordering::Release);
    }
}

/// 弹窗被替换或撤销时，精确结清待决的风险选择。
///
/// 这不是渲染兜底：确认可能在首帧渲染之前就被其他 popup 覆盖，此时
/// [`RiskDisplay`] 尚未建立、没有 Drop 可依赖。残留 payload 会一直持有响应
/// 通道，使等待用户回答的会话操作永久占住 operation gate，因此替换/撤销边界
/// 必须显式按取消收敛。
pub(crate) fn cancel_pending_risk_choice() -> bool {
    let state = CONFIRM_PAYLOAD.state();
    let mut payload = state.write();
    let pending = payload
        .as_ref()
        .is_some_and(|p| matches!(p.pending_action, ConfirmAction::RiskChoice(_)));
    if !pending {
        return false;
    }
    let taken = payload.take().expect("risk choice payload checked");
    drop(payload);
    if let ConfirmAction::RiskChoice(owner) = taken.pending_action {
        owner.answer(false);
    }
    true
}

struct RiskPopupGuard(std::sync::Weak<RiskConfirmation>);
impl Drop for RiskPopupGuard {
    fn drop(&mut self) {
        let Some(owner) = self.0.upgrade() else {
            return;
        };
        owner.answer(false);
        // 先释放 payload 锁再动 POPUP_KIND——与 `confirm_risk_choice` 的
        // popup→payload 顺序保持单一方向，避免两个方向同时持锁。
        let cleared = {
            let state = CONFIRM_PAYLOAD.state();
            let mut payload = state.write();
            let mine = payload.as_ref().is_some_and(|p| {
                matches!(&p.pending_action,
                ConfirmAction::RiskChoice(current) if std::sync::Arc::ptr_eq(current, &owner))
            });
            if mine {
                *payload = None;
            }
            mine
        };
        let still_open = { *atoms::POPUP_KIND.state().read() == Some(atoms::PopupKind::Confirm) };
        if cleared && still_open {
            *atoms::POPUP_KIND.state().write() = None;
        }
    }
}

/// 请求一次显式风险选择，直到用户作出选择、弹窗被替换/撤销或确认内容未完整渲染。
///
/// 已有的确认载荷按取消收敛（不能抢占）：同时只允许一个风险选择在等待。
///
/// 返回值区分「用户拒绝」与「用户没拿到机会」：调用方对后者的处理不能是把失败吞掉
/// （`NotShown` 说明该次询问从未成立），对前者才是「用户的结论就是最终结论」。
pub(crate) async fn confirm_risk_choice(prompt: RiskPrompt) -> RiskChoice {
    let (owner, rx) = RiskConfirmation::new(prompt);
    {
        let popup = atoms::POPUP_KIND.state();
        let mut popup = popup.write();
        let payload = CONFIRM_PAYLOAD.state();
        let mut payload = payload.write();
        if popup.is_some() || payload.is_some() {
            // 位置被占用：这次询问没有发生，也不能抢占别人的弹窗。
            return RiskChoice::NotShown;
        }
        let (body, _) = owner.prompt.body();
        *payload = Some(atoms::ConfirmPayload {
            title: body[0].clone(),
            message: body[1].clone(),
            details: body[2..].to_vec(),
            pending_action: ConfirmAction::RiskChoice(owner.clone()),
        });
        *popup = Some(atoms::PopupKind::Confirm);
    }
    let _guard = RiskPopupGuard(std::sync::Arc::downgrade(&owner));
    drop(owner);
    rx.await.unwrap_or(RiskChoice::NotShown)
}

struct RiskDisplay {
    owner: std::sync::Arc<RiskConfirmation>,
    width: u16,
    height: u16,
    area: Option<ratatui_kit::ratatui::layout::Rect>,
}
impl RiskDisplay {
    fn record_area(&mut self, area: ratatui_kit::ratatui::layout::Rect) {
        let visible = area.width >= self.width && area.height >= self.height;
        self.area = visible.then_some(area);
        self.owner
            .displayed
            .store(visible, std::sync::atomic::Ordering::Release);
        // 装不下**不结清**这次选择：窗口尺寸变了下一帧就可能装得下，而这里结清只会
        // 让用户连一次机会都没有（旧行为：立刻按取消收敛 → 用户看到的是「什么都没
        // 发生，会话创建被拒绝」）。替用户接受风险仍被挡住——`RiskConfirmation::answer`
        // 在可见性为假时把「接受」降级为未呈现，因此等待方只能得到 NotShown。
    }
}
impl Hook for RiskDisplay {
    fn pre_component_draw(&mut self, drawer: &mut ComponentDrawer) {
        self.record_area(drawer.area);
    }
}
impl Drop for RiskDisplay {
    fn drop(&mut self) {
        self.owner.answer(false);
    }
}

fn risk_choice(
    event: &Event,
    selected: &mut bool,
    area: Option<ratatui_kit::ratatui::layout::Rect>,
    options: (u16, u16),
) -> Option<bool> {
    match event {
        Event::Key(key) if key.kind == KeyEventKind::Press && key.modifiers.is_empty() => {
            match key.code {
                KeyCode::Esc => Some(false),
                KeyCode::Up | KeyCode::Down | KeyCode::Tab => {
                    *selected = !*selected;
                    None
                }
                KeyCode::Enter => Some(*selected),
                _ => None,
            }
        }
        Event::Mouse(mouse) if mouse.kind == MouseEventKind::Down(MouseButton::Left) => area
            .filter(|rect| rect.contains((mouse.column, mouse.row).into()))
            .and_then(|rect| match mouse.row.checked_sub(rect.y) {
                Some(row) if row == options.0 => Some(false),
                Some(row) if row == options.1 => Some(true),
                _ => None,
            }),
        _ => None,
    }
}

#[derive(Default, Props)]
pub struct RiskPopupProps {
    pub owner: Option<std::sync::Arc<RiskConfirmation>>,
}

#[component]
pub fn RiskPopup(props: &RiskPopupProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let owner = props
        .owner
        .as_ref()
        .expect("risk choice owner required")
        .clone();
    let theme = hooks.use_atom(&THEME_ATOM);
    let _lang = hooks.use_atom(&LANG_VERSION);
    let selected = hooks.use_state(|| false);
    // 选项行随选择变化，必须在这里读 state 才能订阅重渲染。
    let texts = owner.prompt.lines(*selected.read());
    let options = owner.prompt.option_rows();
    let width = texts
        .iter()
        .map(|s| unicode_width::UnicodeWidthStr::width(s.as_str()))
        .max()
        .unwrap_or(0)
        .saturating_add(2)
        .min(u16::MAX as usize) as u16;
    let height = owner.prompt.popup_height();
    let area = {
        let tracker = hooks.use_hook(|| RiskDisplay {
            owner: owner.clone(),
            width,
            height,
            area: None,
        });
        tracker.width = width;
        tracker.height = height;
        tracker.area
    };
    let action_owner = owner.clone();
    hooks.use_event_handler_with_options(
        EventScope::Current,
        EventPriority::High,
        EventOptions { hit_test: true },
        move |event| {
            let mut choice = *selected.read();
            let answer = risk_choice(&event, &mut choice, area, options);
            if choice != *selected.read() {
                *selected.write() = choice;
            }
            if let Some(accepted) = answer {
                action_owner.answer(accepted);
                close_popup();
            }
            // 风险选择期间禁止按键落入背景输入或快速切换快捷键。
            EventResult::Consumed
        },
    );
    let guard = theme.read();
    let lines: Vec<Line<'static>> = texts.into_iter().map(Line::from).collect();
    let paragraph = ratatui_kit::ratatui::widgets::Paragraph::new(lines)
        .style(ratatui_kit::ratatui::style::Style::new().fg(guard.semantic.text.primary))
        .block(ratatui_kit::ratatui::widgets::Block::default().borders(
            ratatui_kit::ratatui::widgets::Borders::TOP
                | ratatui_kit::ratatui::widgets::Borders::BOTTOM,
        ));
    element!(View(width: Constraint::Fill(1), height: Constraint::Fill(1)) { Text(text: paragraph) })
}

#[component]
pub fn ConfirmPopup(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme_def = hooks.use_atom(&THEME_ATOM);
    let payload_store = hooks.use_atom(&CONFIRM_PAYLOAD);
    let payload = payload_store.read().clone();
    let _ = payload_store;

    // 弹窗绘制区域（上一帧）——鼠标整窗点击 = 确认
    let area;
    {
        let tracker = hooks.use_hook(AreaTracker::new);
        area = tracker.rect;
    }

    // 确认动作：Enter 与鼠标左键点击共用（click as enter）
    let confirm = move || {
        // 执行确认逻辑
        if let Some(ref p) = *CONFIRM_PAYLOAD.state().read() {
            execute_confirm_action(&p.pending_action, |action| {
                if let Some(tx) = ASK_USER_RESPONSE_TX.get() {
                    let _ = tx.send(action);
                }
            });
        }
        // 清空确认弹窗 payload 并关闭弹窗
        *CONFIRM_PAYLOAD.state().write() = None;
        close_popup();
    };

    hooks.use_event_handler_with_options(
        EventScope::Current,
        EventPriority::High,
        EventOptions { hit_test: true },
        move |event| {
            // 鼠标：区域内左键点击 = 执行确认动作（click as enter）
            if let Event::Mouse(mouse) = event {
                if area.is_some() && mouse.kind == MouseEventKind::Down(MouseButton::Left) {
                    confirm();
                    return EventResult::Consumed;
                }
                return EventResult::Ignored;
            }
            let Event::Key(key) = event else {
                return EventResult::Ignored;
            };
            if key.kind != KeyEventKind::Press {
                return EventResult::Ignored;
            }
            match (key.modifiers, key.code) {
                (KeyModifiers::NONE, KeyCode::Enter) => {
                    confirm();
                    EventResult::Consumed
                }
                (KeyModifiers::NONE, KeyCode::Esc) => {
                    // 用户选择返回继续作答
                    *CONFIRM_PAYLOAD.state().write() = None;
                    close_popup();
                    EventResult::Consumed
                }
                _ => EventResult::Ignored,
            }
        },
    );
    let _ = hooks.use_atom(&LANG_VERSION);

    let popup_tokens = &theme_def.read().component.popup;
    let guard = theme_def.read();
    let semantic = &guard.semantic;
    let mut lines: Vec<Line<'_>> = Vec::new();

    match &payload {
        None => {
            lines.push(Line::from(""));
            lines.push(
                Line::from(i18n::tr("popup-confirm-empty"))
                    .fg(semantic.text.muted)
                    .italic(),
            );
            lines.push(Line::from(""));
            lines.push(Line::from(i18n::tr("common-esc-close")).fg(semantic.text.dim));
        }
        Some(p) => {
            lines.push(Line::from(""));
            lines.push(
                Line::from(format!("  {}", p.title))
                    .fg(popup_tokens.action_primary)
                    .bold(),
            );
            lines.push(Line::from(""));
            lines.push(Line::from(p.message.clone()).fg(semantic.text.primary));
            for detail in &p.details {
                lines.push(Line::from(detail.clone()).fg(semantic.text.muted));
            }
            lines.push(Line::from(""));
            lines.push(Line::from(i18n::tr("popup-confirm-action-hint")).fg(semantic.text.dim));
        }
    }

    let popup_block = ratatui_kit::ratatui::widgets::Block::default()
        .borders(
            ratatui_kit::ratatui::widgets::Borders::TOP
                | ratatui_kit::ratatui::widgets::Borders::BOTTOM,
        )
        .border_style(ratatui_kit::ratatui::style::Style::new().fg(popup_tokens.border))
        .title_top(
            Line::from(i18n::tr("popup-confirm-title"))
                .fg(popup_tokens.action_primary)
                .bold()
                .centered(),
        );
    let text_render = ratatui_kit::ratatui::widgets::Paragraph::new(
        ratatui_kit::ratatui::text::Text::from(lines),
    )
    .block(popup_block);

    element!(
        View(
            flex_direction: ratatui_kit::ratatui::layout::Direction::Vertical,
            width: ratatui_kit::ratatui::layout::Constraint::Fill(1),
            height: ratatui_kit::ratatui::layout::Constraint::Fill(1),
        ) {
            Text(text: text_render)
        }
    )
}
pub(crate) fn execute_confirm_action(
    action: &ConfirmAction,
    mut send_ask_user: impl FnMut(AskUserResponseAction),
) {
    match action {
        // 通用确认动作绝不能代替专用风险选择。
        ConfirmAction::RiskChoice(owner) => owner.answer(false),
        ConfirmAction::ThreadSwitch(target_id) => {
            if let Some(tx) = atoms::THREAD_LOAD_TX.get() {
                let _ = tx.send(target_id.clone());
            }
        }
        ConfirmAction::RejectAskUser {
            owner,
            request_id_json,
        } => {
            send_ask_user(AskUserResponseAction::Reject {
                owner: owner.clone(),
                request_id_str: request_id_json.clone(),
            });
            crate::kit::panel_registry::close_ask_user_panel_for_owner(owner);
        }
    }
}

#[cfg(test)]
#[path = "risk_choice_test.rs"]
mod recovery_tests;

#[cfg(test)]
#[path = "confirm_popup_test.rs"]
mod tests;
