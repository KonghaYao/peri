//! 当前 session Goal 的只读详情面板。
//!
//! hook 顺序（TUI-HOOK-001）：`hooks.use_*` 无条件排在函数体前部，条件分支与提前
//! `return` 一律排在其后——某条分支少调一个 hook，框架就会按下标取到错位的 hook
//! 而 panic（ratatui-kit `Hooks::use_hook`）。

use crate::app::panel_types::PanelKind;
use crate::i18n;
use crate::kit::atoms::{GOAL_SNAPSHOT, LANG_VERSION};
use crate::kit::panel_mouse::AreaTracker;
use peri_acp_types::goal::GoalStatus;
use peri_theme::atoms::THEME_ATOM;
use ratatui_kit::{
    crossterm::event::{Event, KeyCode, KeyEventKind},
    prelude::*,
    ratatui::{
        layout::Constraint,
        style::{Style, Stylize},
        text::{Line, Span},
        widgets::Paragraph,
    },
};

#[component]
pub fn GoalPanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme_def = hooks.use_atom(&THEME_ATOM);
    let _lang = hooks.use_atom(&LANG_VERSION);
    let goal_store = hooks.use_atom(&GOAL_SNAPSHOT);
    let goal = goal_store.read().clone();
    let sv = hooks.use_state(ScrollViewState::default);

    // 面板绘制区域（上一帧实测）——滚轮仲裁注册的唯一区域来源；首帧未测量时为
    // `None`（注册退化为 0 尺寸），下一帧起生效。
    let area;
    {
        let tracker = hooks.use_hook(AreaTracker::new);
        area = tracker.rect;
    }

    hooks.use_event_handler(EventScope::Current, EventPriority::Normal, move |event| {
        let Event::Key(key) = event else {
            return EventResult::Ignored;
        };
        if key.kind != KeyEventKind::Press {
            return EventResult::Ignored;
        }
        if key.code == KeyCode::Esc {
            close_panel();
        }
        EventResult::Consumed
    });

    #[cfg(test)]
    tests::visit_render_frame();

    let theme = theme_def.read();
    let title = Style::new().fg(theme.component.panel.title).bold();
    let label = Style::new().fg(theme.semantic.text.muted);
    let value = Style::new().fg(theme.semantic.text.primary);
    let mut lines = Vec::new();

    if let Some(goal) = goal {
        lines.push(Line::from(Span::styled(
            goal.objective.unwrap_or_else(|| i18n::tr("ui-empty")),
            title,
        )));
        lines.push(Line::from(""));
        lines.push(detail_line(
            "goal-detail-status",
            status_label(goal.status),
            label,
            value,
        ));
        lines.push(detail_line(
            "goal-detail-continuations",
            goal.continuation_count.to_string(),
            label,
            value,
        ));
        if let Some(reason) = goal
            .blocked_reason
            .filter(|reason| !reason.trim().is_empty())
        {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                i18n::tr("goal-detail-blocked-reason"),
                label,
            )));
            lines.push(Line::from(Span::styled(reason, value)));
        }
    } else {
        lines.push(Line::from(Span::styled(
            i18n::tr("goal-detail-empty"),
            label,
        )));
    }
    drop(theme);

    // 面板滚轮仲裁注册（每帧覆盖写入；区域取自 `AreaTracker` 的上一帧实测区域，
    // 首帧未测量时为 0 尺寸，下一帧起生效）
    crate::kit::panel_scroll::register_panel_scroll(PanelKind::Goal, area.unwrap_or_default(), sv);
    panel_shell!(PanelKind::Goal, {
        ScrollView(
            scrollbars: crate::kit::panel_registry::clean_scrollbars(),
            state: Some(sv),
            width: Constraint::Fill(1),
            height: Constraint::Fill(1),
        ) {
            Text(text: Paragraph::new(ratatui::text::Text::from(lines)).wrap(
                ratatui::widgets::Wrap { trim: false }
            ))
        }
    })
}

fn detail_line(key: &str, text: String, label_style: Style, value_style: Style) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{}: ", i18n::tr(key)), label_style),
        Span::styled(text, value_style),
    ])
}

fn status_label(status: Option<GoalStatus>) -> String {
    match status {
        Some(GoalStatus::Active) => i18n::tr("goal-status-active"),
        Some(GoalStatus::Complete) => i18n::tr("goal-status-complete"),
        Some(GoalStatus::Blocked) => i18n::tr("goal-status-blocked"),
        None => i18n::tr("ui-empty"),
    }
}

fn close_panel() {
    crate::kit::panel_registry::close_active_panel();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kit::atoms::GoalSnapshot;
    use ratatui_kit::ratatui::buffer::Buffer;
    use serial_test::serial;

    fn goal_with(objective: &str, blocked_reason: Option<&str>) -> GoalSnapshot {
        GoalSnapshot {
            objective: Some(objective.to_string()),
            status: Some(GoalStatus::Blocked),
            token_budget: None,
            tokens_used: 0,
            time_used_seconds: 0,
            continuation_count: 1,
            blocked_reason: blocked_reason.map(str::to_string),
        }
    }

    /// `GOAL_SNAPSHOT` 的保存/恢复——断言失败也不污染其它测试。
    struct GoalGuard(Option<GoalSnapshot>);

    impl GoalGuard {
        fn install(goal: Option<GoalSnapshot>) -> Self {
            let previous = std::mem::take(&mut *GOAL_SNAPSHOT.state().write());
            *GOAL_SNAPSHOT.state().write() = goal;
            Self(previous)
        }
    }

    impl Drop for GoalGuard {
        fn drop(&mut self) {
            *GOAL_SNAPSHOT.state().write() = std::mem::take(&mut self.0);
        }
    }

    /// goal 面板没有需要投喂事件的本地状态；探针只提供逐帧回调，让测试在同一实例内
    /// 切换 `GOAL_SNAPSHOT`（有 goal ⇄ 无 goal）以验证 hook 序列不随分支变化。
    type FrameVisitor = Option<Box<dyn FnMut()>>;

    thread_local! {
        static FRAME_VISITOR: std::cell::RefCell<FrameVisitor> =
            const { std::cell::RefCell::new(None) };
    }

    /// 卸载时清空回调：断言 panic 也不把回调泄漏给同线程的后续用例。
    struct VisitorGuard;

    impl Drop for VisitorGuard {
        fn drop(&mut self) {
            FRAME_VISITOR.with(|slot| *slot.borrow_mut() = None);
        }
    }

    pub(super) fn visit_render_frame() {
        FRAME_VISITOR.with(|slot| {
            if let Some(visitor) = slot.borrow_mut().as_mut() {
                visitor();
            }
        });
    }

    /// 挂载并渲染 `frames` 帧，返回末帧缓冲；每帧回调一次 visitor。
    fn mount_panel_frames(frames: usize, visitor: impl FnMut() + 'static) -> Buffer {
        let _guard = VisitorGuard;
        FRAME_VISITOR.with(|slot| {
            assert!(slot.borrow().is_none(), "上一个 visitor 未清理");
            *slot.borrow_mut() = Some(Box::new(visitor));
        });
        ratatui_kit::test_util::render_frames(element!(super::GoalPanel()), 70, 18, frames)
    }

    fn buffer_text(buf: &Buffer) -> String {
        let mut out = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn status_labels_cover_all_states() {
        assert_eq!(
            status_label(Some(GoalStatus::Active)),
            i18n::tr("goal-status-active")
        );
        assert_eq!(
            status_label(Some(GoalStatus::Complete)),
            i18n::tr("goal-status-complete")
        );
        assert_eq!(
            status_label(Some(GoalStatus::Blocked)),
            i18n::tr("goal-status-blocked")
        );
        assert_eq!(status_label(None), i18n::tr("ui-empty"));
    }

    /// 空态可见：没有 goal 时渲染空态文案，而不是一片空白。
    #[test]
    #[serial]
    fn empty_state_is_rendered() {
        let _goal = GoalGuard::install(None);
        let buf = mount_panel_frames(2, || {});
        let text = buffer_text(&buf);
        assert!(
            text.contains(&i18n::tr("goal-detail-empty")),
            "无 goal 时必须渲染空态文案：\n{text}"
        );
    }

    /// 异常态可见：blocked 原因必须原样渲染，不得被吞掉。
    #[test]
    #[serial]
    fn blocked_reason_is_rendered() {
        let _goal =
            GoalGuard::install(Some(goal_with("ship the refactor", Some("quota exceeded"))));
        let buf = mount_panel_frames(2, || {});
        let text = buffer_text(&buf);
        assert!(
            text.contains("ship the refactor"),
            "objective 必须渲染：\n{text}"
        );
        assert!(
            text.contains("quota exceeded"),
            "blocked_reason 必须渲染：\n{text}"
        );
    }

    /// hook 顺序（TUI-HOOK-001）：有 goal ⇄ 无 goal 在同一实例内逐帧切换，hook 序列
    /// 必须稳定——某条分支改变 hook 数量时框架会按下标取到错位 hook 并 panic。
    #[test]
    #[serial]
    fn hook_sequence_survives_goal_branch_flip() {
        let _goal = GoalGuard::install(Some(goal_with("first", None)));
        // `frame` 是 Copy，move 进回调的是副本——外层计数必须经 `Rc<Cell>` 观察
        let rendered = std::rc::Rc::new(std::cell::Cell::new(0usize));
        let counter = rendered.clone();
        let mut frame = 0usize;
        let buf = mount_panel_frames(4, move || {
            // 第 1 帧后清空 goal：第 2 帧起走空态分支（渲染体读 atom 早于本回调）
            if frame == 1 {
                *GOAL_SNAPSHOT.state().write() = None;
            }
            frame += 1;
            counter.set(counter.get() + 1);
        });
        assert!(
            rendered.get() >= 3,
            "面板必须在分支切换后继续渲染，实际 {} 帧",
            rendered.get()
        );
        let text = buffer_text(&buf);
        assert!(
            text.contains(&i18n::tr("goal-detail-empty")),
            "分支切换后必须渲染空态文案：\n{text}"
        );
    }
}
