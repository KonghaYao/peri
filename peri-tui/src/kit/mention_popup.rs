//! MentionPopup：@ 提及补全弹窗。
//!
//! 输入区本地 owner：方向键/确认/取消与鼠标回调都留在这里，
//! 列表绘制、可见窗口与命中交给 `peri_tui_components::completion::CompletionPopup`。

use std::sync::{Arc, Mutex};

use fuzzy_matcher::FuzzyMatcher;
use fuzzy_matcher::skim::SkimMatcherV2;

use ratatui_kit::crossterm::event::Event;
use ratatui_kit::crossterm::event::KeyEventKind;
use ratatui_kit::crossterm::event::MouseButton;
use ratatui_kit::crossterm::event::MouseEventKind;
use ratatui_kit::prelude::*;
use ratatui_kit::ratatui::layout::Constraint;
use ratatui_kit::ratatui::layout::Direction;
use ratatui_kit::ratatui::style::Modifier;
use ratatui_kit::ratatui::style::Style;
use ratatui_kit::ratatui::style::Stylize;
use ratatui_kit::ratatui::text::Line;

use crate::i18n;

use crate::kit::inline_nav::InlineNavAction;
use fluent_bundle::FluentValue;
use peri_theme::atoms::THEME_ATOM;
use peri_tui_components::completion::{
    CompletionPopup, CompletionRow, CompletionStyles, MAX_CANDIDATES,
};
use peri_tui_components::panel_mouse::AreaTracker;

#[derive(Default, Props)]
pub struct MentionPopupProps {
    pub prefix: String,
    pub items: Vec<String>,
    pub on_select: Arc<Mutex<Handler<'static, String>>>,
    pub on_cancel: Arc<Mutex<Handler<'static, ()>>>,
}

/// 候选：空 prefix 取前 [`MAX_CANDIDATES`] 条；否则按 fuzzy 分数排序后截断。
///
/// 搜索是宿主策略，弹窗只窗口化这里给出的行。
pub(crate) fn mention_candidates(items: &[String], prefix: &str) -> Vec<String> {
    if prefix.is_empty() {
        return items.iter().take(MAX_CANDIDATES).cloned().collect();
    }
    let matcher = SkimMatcherV2::default();
    let query = prefix.to_lowercase();
    let mut scored: Vec<(i64, &String)> = items
        .iter()
        .filter_map(|item| {
            let score = matcher.fuzzy_match(item, &query)?;
            Some((score, item))
        })
        .collect();
    // 按分数降序排列
    scored.sort_by_key(|b| std::cmp::Reverse(b.0));
    scored
        .into_iter()
        .take(MAX_CANDIDATES)
        .map(|(_, item)| item.clone())
        .collect()
}

/// Confirm（Enter）落点：空候选关闭弹窗，否则选中钳制后的条目。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MentionConfirm {
    Select(usize),
    Cancel,
}

pub(crate) fn confirm_outcome(selection: usize, item_count: usize) -> MentionConfirm {
    if item_count == 0 {
        MentionConfirm::Cancel
    } else {
        MentionConfirm::Select(crate::kit::inline_nav::clamp_selection(
            selection, item_count,
        ))
    }
}

/// 执行 Confirm 结果；状态读锁已在此之前释放，回调里可以安全写回同一 atom。
fn run_confirm<T: Clone>(
    outcome: MentionConfirm,
    items: &[T],
    mut on_select: impl FnMut(T),
    mut on_cancel: impl FnMut(),
) {
    match outcome {
        MentionConfirm::Select(index) => match items.get(index) {
            Some(item) => on_select(item.clone()),
            // 防御：钳制后仍越界（条目表在两帧之间收缩）
            None => on_cancel(),
        },
        MentionConfirm::Cancel => on_cancel(),
    }
}

#[component]
pub fn MentionPopup(props: &MentionPopupProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let selection = hooks.use_atom(&MENTION_SELECTED_INDEX);

    let filtered = mention_candidates(&props.items, &props.prefix);
    let item_count = filtered.len();
    let filtered_for_handler = filtered.clone();
    let on_select = Arc::clone(&props.on_select);
    let on_cancel = Arc::clone(&props.on_cancel);

    // 弹窗绘制区域（上一帧）——鼠标点击行号反推
    let area;
    {
        let tracker = hooks.use_hook(AreaTracker::new);
        area = tracker.rect;
    }

    let (popup_h, title_style, styles, label_color) = {
        let state = THEME_ATOM.state();
        let guard = state.read();
        let popup_tokens = &guard.component.popup;
        (
            popup_tokens.inline_height,
            Style::default()
                .fg(popup_tokens.action_primary)
                .add_modifier(Modifier::BOLD),
            CompletionStyles {
                border: popup_tokens.border,
                selected_label: Style::default()
                    .fg(popup_tokens.selected_fg)
                    .add_modifier(Modifier::BOLD),
                selected_detail: Style::default().fg(popup_tokens.selected_fg),
                empty: Style::default().fg(guard.semantic.text.muted),
            },
            guard.semantic.text.primary,
        )
    };

    let rows: Vec<CompletionRow<'static>> = filtered
        .iter()
        .map(|item| CompletionRow::new(Line::from(item.clone()).fg(label_color)))
        .collect();
    let popup = CompletionPopup {
        rows,
        selected: *selection.read(),
        height: popup_h,
        title: Line::from(i18n::tr_args(
            "mention-popup-title",
            &[("name".to_string(), FluentValue::from(props.prefix.as_str()))],
        ))
        .patch_style(title_style),
        empty_text: Line::from(i18n::tr("common-no-matches")),
        styles,
    };
    // 绘制与命中共用同一次视口计算（候选变化/缩放后由下一帧整体重建）。
    let hit_layout = popup.hit_layout();

    hooks.use_event_handler_with_options(
        EventScope::Current,
        EventPriority::Normal,
        EventOptions { hit_test: true },
        move |event| {
            // 鼠标：区域内左键点击 = 选中该项并执行 Enter 动作（click as enter）
            if let Event::Mouse(mouse) = event {
                if let Some(area) = area
                    && let Some(idx) =
                        peri_tui_components::panel_mouse::hit_item(&mouse, area, hit_layout)
                {
                    *selection.write() = idx;
                    if let Some(item) = filtered_for_handler.get(idx).cloned() {
                        let mut on_select =
                            on_select.lock().expect("MentionPopup on_select poisoned");
                        (*on_select)(item);
                    } else {
                        let mut on_cancel =
                            on_cancel.lock().expect("MentionPopup on_cancel poisoned");
                        (*on_cancel)(());
                    }
                    return EventResult::Consumed;
                }
                // 区域内的左键点击（未命中行）也消费，防止穿透到输入区
                return match mouse.kind {
                    MouseEventKind::Down(MouseButton::Left) => EventResult::Consumed,
                    _ => EventResult::Ignored,
                };
            }
            let Event::Key(key) = event else {
                return EventResult::Ignored;
            };
            if key.kind != KeyEventKind::Press {
                return EventResult::Ignored;
            }
            match crate::kit::inline_nav::classify_inline_nav(&key) {
                Some(InlineNavAction::MoveUp) => {
                    let mut s = selection.write();
                    *s = crate::kit::inline_nav::previous_selection(*s);
                    EventResult::Consumed
                }
                Some(InlineNavAction::MoveDown) => {
                    let mut s = selection.write();
                    *s = crate::kit::inline_nav::next_selection(*s, item_count);
                    EventResult::Consumed
                }
                Some(InlineNavAction::Confirm) => {
                    // 读锁在语句结束时释放，回调里再写 atom 不会自锁。
                    let outcome = confirm_outcome(*selection.read(), item_count);
                    run_confirm(
                        outcome,
                        &filtered_for_handler,
                        |item| {
                            let mut on_select =
                                on_select.lock().expect("MentionPopup on_select poisoned");
                            (*on_select)(item);
                        },
                        || {
                            let mut on_cancel =
                                on_cancel.lock().expect("MentionPopup on_cancel poisoned");
                            (*on_cancel)(());
                        },
                    );
                    EventResult::Consumed
                }
                Some(InlineNavAction::Cancel) => {
                    let mut on_cancel = on_cancel.lock().expect("MentionPopup on_cancel poisoned");
                    (*on_cancel)(());
                    EventResult::Consumed
                }
                None => EventResult::Ignored,
            }
        },
    );

    element!(
        View(
            flex_direction: Direction::Vertical,
            width: Constraint::Fill(1),
            height: Constraint::Length(popup_h),
        ) {
            Text(text: popup.widget())
        }
    )
}

use crate::kit::atoms::MENTION_SELECTED_INDEX;

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    fn items(count: usize) -> Vec<String> {
        (0..count).map(|i| format!("src/file-{i:02}.rs")).collect()
    }

    /// 保留 @ 的 20 条候选上限（候选条数与模糊命中两条路径）。
    #[test]
    fn test_mention_candidates_cap_at_twenty() {
        let all = items(30);
        let empty_prefix = mention_candidates(&all, "");
        assert_eq!(empty_prefix.len(), MAX_CANDIDATES);
        assert_eq!(empty_prefix[19], "src/file-19.rs");

        let matched = mention_candidates(&all, "file");
        assert_eq!(matched.len(), MAX_CANDIDATES);
    }

    /// 空候选 Enter：关闭弹窗，不选中任何条目。
    #[test]
    fn test_empty_candidates_confirm_cancels() {
        assert_eq!(confirm_outcome(3, 0), MentionConfirm::Cancel);

        let log = RefCell::new(Vec::new());
        run_confirm(
            confirm_outcome(3, 0),
            &[] as &[String],
            |item| log.borrow_mut().push(format!("select:{item}")),
            || log.borrow_mut().push("cancel".to_string()),
        );
        assert_eq!(*log.borrow(), ["cancel"]);
    }

    /// 有候选时 Confirm 选中钳制后的条目，取消回调不得触发。
    #[test]
    fn test_confirm_selects_clamped_item() {
        let all = vec![String::from("src/a.rs"), String::from("src/b.rs")];
        let log = RefCell::new(Vec::new());
        run_confirm(
            confirm_outcome(99, all.len()),
            &all,
            |item| log.borrow_mut().push(format!("select:{item}")),
            || log.borrow_mut().push("cancel".to_string()),
        );
        assert_eq!(*log.borrow(), ["select:src/b.rs"]);
    }
}
