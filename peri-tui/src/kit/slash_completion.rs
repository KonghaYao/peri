//! SlashCompletion：斜杠命令补全弹窗。
//!
//! SlashCompletion：输入区本地 owner，自己处理方向键/确认/取消，
//! 通过回调把选择结果反馈给 InputArea。

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
use ratatui_kit::ratatui::text::Span;

use crate::i18n;

use crate::kit::inline_nav::InlineNavAction;
use crate::kit::slash_projection::ArgsSchema;
use fluent_bundle::FluentValue;
use peri_theme::atoms::THEME_ATOM;
use peri_tui_components::completion::{CompletionPopup, CompletionRow, CompletionStyles};
use peri_tui_components::panel_mouse::AreaTracker;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlashActionKind {
    Panel,
    Command,
    Skill,
    McpSkill,
}

/// Slash 条目按 kind 的静态语义色（S16：三层 slash 用颜色区分，不用方括号标签）。
/// McpSkill 用 `semantic.model_info`（spec 允许「主题中专门 token」，
/// `semantic.status.info` 不存在——不加 peri-theme 新字段，零范围蔓延）。
#[must_use]
pub(crate) fn slash_kind_color(
    kind: &SlashActionKind,
    semantic: &peri_theme::semantic::SemanticTokens,
) -> ratatui::style::Color {
    match kind {
        SlashActionKind::Panel => semantic.border.active,
        SlashActionKind::Command => semantic.text.muted,
        SlashActionKind::Skill => semantic.status.warning,
        SlashActionKind::McpSkill => semantic.model_info,
    }
}

#[derive(Debug, Clone)]
pub struct SlashCompletionItem {
    /// 显示形态（映射时已按 level 变换：1 裸名 / 2 全名）。
    pub label: String,
    /// 提交形态（display 即 lexical：== label，解析器严格命中）。
    pub insert_text: String,
    pub description: String,
    pub kind: SlashActionKind,
    /// label 的小写版本，预计算避免每帧 to_lowercase() 分配。
    pub label_lowercase: String,
    /// 元数据：唯一键（ui 域解析 / fuzzy 双索引 / args 关联）。
    pub fullname: String,
    /// 参数 schema（schema 驱动补全 / 校验，步骤 9 消费）。
    pub args: Option<ArgsSchema>,
    /// 双索引匹配串（Phase 4 步骤 4 预计算）：label_lowercase + fullname_lowercase
    /// 合并——level 1 裸名条目也可被全名前缀（如 `/demo:`）模糊搜到（R4）。
    pub search_lowercase: String,
}

impl SlashCompletionItem {
    /// 预计算双索引匹配串：label_lowercase + fullname_lowercase 合并。
    /// fullname 为空（过渡期合成条目）时退化为 label 小写。
    #[must_use]
    pub fn make_search_lowercase(label_lowercase: &str, fullname: &str) -> String {
        if fullname.is_empty() {
            label_lowercase.to_string()
        } else {
            format!("{label_lowercase} {}", fullname.to_lowercase())
        }
    }
}

#[derive(Default, Props)]
pub struct SlashCompletionProps {
    pub prefix: String,
    pub items: Vec<SlashCompletionItem>,
    pub on_select: Arc<Mutex<Handler<'static, SlashCompletionItem>>>,
    pub on_cancel: Arc<Mutex<Handler<'static, ()>>>,
    /// 无候选（列表空 / prefix 无匹配）时 Confirm 的落点：关闭弹窗后把 Enter
    /// 交回**提交通道**（输入区 `submit_text` 落点）。
    ///
    /// \[Why\] 本组件在同一输入层、同优先级下晚于输入区注册；输入区的 Enter 提交
    /// 分支带 `!slash_active` 守卫（peri-tui/src/kit/input_area.rs），事件只会被
    /// 本组件消费一次——无候选时若只 `on_cancel`，用户按下的 Enter 与输入框内容
    /// 会被静默吞掉（`/plugin` 打不开面板）。因此由弹窗自己回调提交，而不依赖
    /// 「取消后再放行给输入区」（同层分发单趟，父组件不会二次收到该事件）。
    pub on_submit: Arc<Mutex<Handler<'static, ()>>>,
}

/// 按 prefix 过滤 + 模糊打分排序（Phase 4 步骤 4 抽出为独立函数便于单测）。
///
/// 双索引：匹配串为预计算 `search_lowercase`（label_lowercase +
/// fullname_lowercase 合并）——level 1 裸名条目也能被全名前缀（如
/// `/demo:`）搜到（R4）；level 2 全名条目经 label 路径命中。
fn filter_slash_items(items: &[SlashCompletionItem], prefix: &str) -> Vec<SlashCompletionItem> {
    if prefix.is_empty() {
        return items.to_vec();
    }
    let matcher = SkimMatcherV2::default();
    let query = prefix.to_lowercase();
    let mut scored: Vec<(i64, SlashCompletionItem)> = items
        .iter()
        .filter_map(|item| {
            let score = matcher.fuzzy_match(&item.search_lowercase, &query)?;
            Some((score, item.clone()))
        })
        .collect();
    // 按模糊匹配分数降序排列
    scored.sort_by_key(|b| std::cmp::Reverse(b.0));
    scored.into_iter().map(|(_, item)| item).collect()
}

/// Confirm（Enter）落点裁决（R5 修复：抽出为纯函数便于单测，同
/// [`filter_slash_items`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConfirmOutcome {
    /// 有可选项：选中该下标（已按 item_count 钳制）。
    Select(usize),
    /// 无候选（列表空 / prefix 无匹配）：关弹窗 + 走提交通道，不吞输入。
    Submit,
}

pub(crate) fn confirm_outcome(selection: usize, item_count: usize) -> ConfirmOutcome {
    if item_count == 0 {
        ConfirmOutcome::Submit
    } else {
        ConfirmOutcome::Select(peri_tui_components::list_nav::clamp_selection(
            selection, item_count,
        ))
    }
}

/// 执行 Confirm 结果；状态读锁已在此之前释放，回调里可以安全写回同一 atom。
fn run_confirm<T: Clone>(
    outcome: ConfirmOutcome,
    items: &[T],
    mut on_select: impl FnMut(T),
    mut on_cancel: impl FnMut(),
    mut on_submit: impl FnMut(),
) {
    match outcome {
        ConfirmOutcome::Select(index) => match items.get(index) {
            Some(item) => on_select(item.clone()),
            // 防御：钳制后下标仍越界（条目表在两帧之间收缩）
            None => on_cancel(),
        },
        ConfirmOutcome::Submit => {
            // 无候选：先关弹窗（清 SLASH_HINT_ACTIVE，输入区守卫恢复），
            // 再走提交通道——Enter 只提交一次，也不被静默吞掉。
            on_cancel();
            on_submit();
        }
    }
}

/// 把条目映射为补全行：label 列宽对齐，描述列独立着色（S16 三层颜色）。
fn slash_rows(
    items: &[SlashCompletionItem],
    semantic: &peri_theme::semantic::SemanticTokens,
) -> Vec<CompletionRow<'static>> {
    // 双列布局：label 列最大宽度（含 / 前缀），描述列自然对齐
    let max_label_width = items
        .iter()
        .map(|item| item.label.chars().count() + 1) // +1 for '/'
        .max()
        .unwrap_or(0);
    items
        .iter()
        .map(|item| {
            let tier_color = slash_kind_color(&item.kind, semantic);
            let padded_label = format!("/{:<width$}", item.label, width = max_label_width);
            CompletionRow::new(Line::from(Span::styled(
                padded_label,
                Style::default().fg(tier_color),
            )))
            .with_detail(Line::from(Span::styled(
                item.description.clone(),
                Style::default().fg(semantic.text.dim),
            )))
        })
        .collect()
}

#[component]
pub fn SlashCompletion(
    props: &SlashCompletionProps,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let selection = hooks.use_atom(&SLASH_SELECTED_INDEX);

    let filtered = filter_slash_items(&props.items, &props.prefix);

    let item_count = filtered.len();
    let filtered_for_handler = filtered.clone();
    let on_select = Arc::clone(&props.on_select);
    let on_cancel = Arc::clone(&props.on_cancel);
    let on_submit = Arc::clone(&props.on_submit);

    // 弹窗绘制区域（上一帧）——鼠标点击行号反推
    let area;
    {
        let tracker = hooks.use_hook(AreaTracker::new);
        area = tracker.rect;
    }

    // 绘制与命中共用同一次视口计算（候选变化/缩放后由下一帧整体重建）。
    let popup = {
        let state = THEME_ATOM.state();
        let guard = state.read();
        let popup_tokens = &guard.component.popup;
        CompletionPopup {
            rows: slash_rows(&filtered, &guard.semantic),
            selected: *selection.read(),
            height: popup_tokens.inline_height,
            title: Line::from(i18n::tr_args(
                "slash-completion-title",
                &[("name".to_string(), FluentValue::from(props.prefix.as_str()))],
            ))
            .fg(popup_tokens.action_primary)
            .bold(),
            empty_text: Line::from(i18n::tr("common-no-matches")),
            styles: CompletionStyles {
                border: popup_tokens.border,
                selected_label: Style::default()
                    .fg(popup_tokens.selected_fg)
                    .add_modifier(Modifier::BOLD),
                selected_detail: Style::default().fg(popup_tokens.selected_fg),
                empty: Style::default().fg(guard.semantic.text.muted),
            },
        }
    };
    let popup_h = popup.height;
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
                        let mut on_select = on_select
                            .lock()
                            .expect("SlashCompletion on_select poisoned");
                        (*on_select)(item);
                    } else {
                        let mut on_cancel = on_cancel
                            .lock()
                            .expect("SlashCompletion on_cancel poisoned");
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
                    *s = peri_tui_components::list_nav::previous_selection(*s);
                    EventResult::Consumed
                }
                Some(InlineNavAction::MoveDown) => {
                    let mut s = selection.write();
                    *s = peri_tui_components::list_nav::next_selection(*s, item_count);
                    EventResult::Consumed
                }
                Some(InlineNavAction::Confirm) => {
                    // [TRAP] 裁决必须先在独立语句里算出：`match confirm_outcome(
                    // *selection.read(), ..)` 会把 `read()` 的临时守卫存活到整个
                    // match 体结束，而下面 on_select/on_cancel 会写回同一 atom
                    // （SLASH_SELECTED_INDEX）——同线程 read→write 自锁，
                    // 主线程卡在 parking_lot wait_for_readers，UI 永久冻结。
                    let outcome = confirm_outcome(*selection.read(), item_count);
                    run_confirm(
                        outcome,
                        &filtered_for_handler,
                        |item| {
                            let mut on_select = on_select
                                .lock()
                                .expect("SlashCompletion on_select poisoned");
                            (*on_select)(item);
                        },
                        || {
                            let mut on_cancel = on_cancel
                                .lock()
                                .expect("SlashCompletion on_cancel poisoned");
                            (*on_cancel)(());
                        },
                        || {
                            let mut on_submit = on_submit
                                .lock()
                                .expect("SlashCompletion on_submit poisoned");
                            (*on_submit)(());
                        },
                    );
                    EventResult::Consumed
                }
                Some(InlineNavAction::Cancel) => {
                    let mut on_cancel = on_cancel
                        .lock()
                        .expect("SlashCompletion on_cancel poisoned");
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

use crate::kit::atoms::SLASH_SELECTED_INDEX;

#[cfg(test)]
mod tests {
    use super::*;
    use peri_theme::prelude::dark_theme;
    use ratatui_kit::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    use ratatui_kit::ratatui::buffer::Buffer;
    use ratatui_kit::ratatui::layout::Rect;
    use ratatui_kit::ratatui::style::Color;
    use ratatui_kit::ratatui::widgets::Widget;

    fn semantic() -> peri_theme::semantic::SemanticTokens {
        dark_theme().semantic
    }

    fn test_styles() -> CompletionStyles {
        CompletionStyles {
            border: Color::Blue,
            selected_label: Style::default().fg(Color::Cyan),
            selected_detail: Style::default().fg(Color::Cyan),
            empty: Style::default().fg(Color::DarkGray),
        }
    }

    fn click(column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: ratatui_kit::crossterm::event::KeyModifiers::NONE,
        }
    }

    #[test]
    fn test_slash_kind_color_mcp_skill_uses_model_info() {
        let s = semantic();
        assert_eq!(
            slash_kind_color(&SlashActionKind::McpSkill, &s),
            s.model_info,
            "McpSkill 应映射 semantic.model_info"
        );
    }

    /// 构造测试条目：label 为显示形态，fullname 为唯一键。
    fn item(label: &str, fullname: &str) -> SlashCompletionItem {
        SlashCompletionItem {
            label: label.to_string(),
            insert_text: label.to_string(),
            description: String::new(),
            kind: SlashActionKind::Command,
            label_lowercase: label.to_lowercase(),
            fullname: fullname.to_string(),
            search_lowercase: SlashCompletionItem::make_search_lowercase(
                &label.to_lowercase(),
                fullname,
            ),
            args: None,
        }
    }

    /// Phase 4 步骤 4：fuzzy 双索引——level 1 裸名条目（label=hello）也能被
    /// 全名前缀 `/demo:` 搜到（fullname 索引路径，R4）。
    #[test]
    fn test_filter_slash_items_fullname_index_matches_level1_bare_label() {
        let items = vec![
            item("hello", "demo:hello"),
            item("compact", "core:compact"),
            item("model", ""),
        ];
        let hits = filter_slash_items(&items, "demo:");
        assert!(
            hits.iter().any(|i| i.fullname == "demo:hello"),
            "/demo: 应经 fullname 索引命中 level 1 裸名条目 demo:hello"
        );
        assert!(hits.iter().all(|i| i.fullname != "core:compact"));
        assert!(hits.iter().all(|i| !i.fullname.is_empty()));
    }

    /// Phase 4 步骤 4：level 2 全名条目（label == fullname）被 `/demo:`
    /// 前缀搜到（label 路径）。
    #[test]
    fn test_filter_slash_items_fullname_prefix_matches_level2_label() {
        let items = vec![
            item("demo:hello", "demo:hello"),
            item("other:bye", "other:bye"),
        ];
        let hits = filter_slash_items(&items, "demo:");
        assert!(
            hits.iter().any(|i| i.fullname == "demo:hello"),
            "/demo: 应命中 level 2 全名条目 demo:hello"
        );
        assert!(hits.iter().all(|i| i.fullname != "other:bye"));
    }

    /// label（裸名）路径仍可命中；空 prefix 返回全部（原行为）。
    #[test]
    fn test_filter_slash_items_label_index_and_empty_prefix() {
        let items = vec![item("hello", "demo:hello"), item("world", "demo:world")];
        assert_eq!(filter_slash_items(&items, "").len(), items.len());
        let hits = filter_slash_items(&items, "hello");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].fullname, "demo:hello");
    }

    /// 回归（空候选 Enter 吞输入）：弹窗激活但**无候选**（列表空 / prefix 无
    /// 匹配，渲染 `common-no-matches`）时 Confirm 必须落到提交分支——只关弹窗
    /// 会把这次 Enter 静默吞掉，`/plugin` 永远打不开面板。
    #[test]
    fn test_confirm_outcome_empty_items_submits() {
        assert_eq!(confirm_outcome(0, 0), ConfirmOutcome::Submit);
        // 上一轮列表残留的选中下标不得让空候选「命中」某一条目
        assert_eq!(confirm_outcome(7, 0), ConfirmOutcome::Submit);
    }

    /// 有候选时 Confirm 仍是选中（含越界下标钳制），提交分支不得抢占。
    #[test]
    fn test_confirm_outcome_with_items_selects_clamped() {
        assert_eq!(confirm_outcome(0, 2), ConfirmOutcome::Select(0));
        assert_eq!(confirm_outcome(1, 2), ConfirmOutcome::Select(1));
        assert_eq!(confirm_outcome(99, 2), ConfirmOutcome::Select(1));
    }

    /// 回归：既有 Panel/Command/Skill 映射不变（渲染行为除新增色外不变）。
    #[test]
    fn test_slash_kind_color_existing_kinds_unchanged() {
        let s = semantic();
        assert_eq!(
            slash_kind_color(&SlashActionKind::Panel, &s),
            s.border.active
        );
        assert_eq!(
            slash_kind_color(&SlashActionKind::Command, &s),
            s.text.muted
        );
        assert_eq!(
            slash_kind_color(&SlashActionKind::Skill, &s),
            s.status.warning
        );
    }

    fn row_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    /// 描述列与三级颜色保持不变：label 按最大宽度对齐，描述另起两列。
    #[test]
    fn test_slash_rows_keep_aligned_label_and_description() {
        let s = semantic();
        let items = vec![
            SlashCompletionItem {
                description: "Show help".to_string(),
                ..item("help", "")
            },
            SlashCompletionItem {
                description: "Swap model".to_string(),
                ..item("model", "")
            },
        ];
        let rows = slash_rows(&items, &s);
        assert_eq!(row_text(&rows[0].label), "/help  ");
        assert_eq!(row_text(&rows[1].label), "/model ");
        let detail = rows[0].detail.as_ref().expect("slash 行必须保留描述列");
        assert_eq!(row_text(detail), "Show help");
        assert_eq!(rows[0].label.spans[0].style.fg, Some(s.text.muted));
        assert_eq!(detail.spans[0].style.fg, Some(s.text.dim));
    }

    /// / 真实候选：超过可视窗口时，绘制行号与点击命中共用同一个窗口。
    #[test]
    fn test_slash_rows_and_hits_share_one_window() {
        let items: Vec<_> = (0..12)
            .map(|index| SlashCompletionItem {
                description: format!("detail-{index:02}"),
                ..item(&format!("cmd{index:02}"), "")
            })
            .collect();
        let filtered = filter_slash_items(&items, "");
        assert_eq!(filtered.len(), items.len());

        let popup = CompletionPopup {
            rows: slash_rows(&filtered, &semantic()),
            selected: 8,
            height: 6,
            title: Line::from(" slash "),
            empty_text: Line::from("no matches"),
            styles: test_styles(),
        };
        let area = Rect::new(0, 4, 44, 6);
        let mut buffer = Buffer::empty(area);
        popup.widget().render(area, &mut buffer);
        let layout = popup.hit_layout();
        assert_eq!(layout.scroll_start, 7, "选中项保持在可视窗口内");

        assert_eq!(popup.hit(&click(2, area.y), area), None);
        assert_eq!(popup.hit(&click(2, area.y + area.height - 1), area), None);
        for visual in 0..layout.visible_items {
            let row = area.y + 1 + visual;
            let painted: String = (area.x..area.x + area.width)
                .map(|x| buffer[(x, row)].symbol())
                .collect();
            match popup.hit(&click(2, row), area) {
                Some(index) => {
                    assert_eq!(index, layout.scroll_start + visual as usize);
                    assert!(
                        painted.contains(&format!("detail-{index:02}")),
                        "row {row} painted {painted:?} but hit index {index}"
                    );
                }
                None => assert!(
                    painted.trim().is_empty(),
                    "row {row} must stay unpainted and unclickable, got {painted:?}"
                ),
            }
        }
    }

    /// 空候选 Enter：先关闭弹窗，再提交输入一次（且只提交一次）。
    #[test]
    fn test_confirm_empty_cancels_then_submits_once() {
        let log = std::cell::RefCell::new(Vec::new());
        run_confirm(
            confirm_outcome(0, 0),
            &[] as &[SlashCompletionItem],
            |_| log.borrow_mut().push("select"),
            || log.borrow_mut().push("cancel"),
            || log.borrow_mut().push("submit"),
        );
        assert_eq!(*log.borrow(), ["cancel", "submit"]);
    }

    /// 有候选时 Confirm 只选中，不关闭也不提交。
    #[test]
    fn test_confirm_with_items_does_not_submit() {
        let items = vec![item("help", "")];
        let log = std::cell::RefCell::new(Vec::new());
        run_confirm(
            confirm_outcome(0, items.len()),
            &items,
            |_| log.borrow_mut().push("select"),
            || log.borrow_mut().push("cancel"),
            || log.borrow_mut().push("submit"),
        );
        assert_eq!(*log.borrow(), ["select"]);
    }
}
