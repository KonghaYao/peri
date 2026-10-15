//! AgentPanel 面板测试：hook 顺序（挂载帧）、cursor 越界收敛与鼠标命中同源。

use super::*;

use crate::kit::atoms::{ServiceSnapshot, ViewModelsSnapshot};
use crate::kit::tui_render_unit::{TuiCollapsedGroup, TuiSubAgentGroup, TuiUserBubble};
use ratatui_kit::crossterm::event::{
    KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui_kit::prelude::{EventResult, State, element};
use ratatui_kit::ratatui::buffer::Buffer;
use ratatui_kit::ratatui::layout::Position;
use serial_test::serial;

/// 测试面板区域：宽 60、高 18（`panel_registry` 中 Agent 的固定布局）。
const TEST_AREA: Rect = Rect {
    x: 0,
    y: 0,
    width: 60,
    height: 18,
};

fn make_subagent(id: &str, name: &str) -> TuiSubAgentGroup {
    TuiSubAgentGroup {
        instance_id: format!("instance-{id}"),
        agent_id: id.to_string(),
        agent_name: name.to_string(),
        view_models: im::Vector::new(),
        collapsed: false,
        is_running: false,
        is_error: false,
        error_reason: None,
        fold: crate::kit::tui_render_unit::FoldState::Collapsed,
        user_modified: false,
        content_hash: 0,
    }
}

#[test]
fn test_agent_wrapped_content_fills_scroll_buffer_to_last_row() {
    let lines = vec![ratatui_kit::ratatui::text::Line::from("你好世界abcd")];
    let content_height = wrapped_content_height(&lines, 5);
    let viewport_height = 2u16;
    let bottom_offset = content_height.saturating_sub(viewport_height);

    assert_eq!(content_height, 3);
    assert!(content_height > viewport_height);
    assert_eq!(bottom_offset + viewport_height, content_height);
}

#[test]
fn test_collect_subagents_empty_snapshot() {
    let snap = ViewModelsSnapshot::default();
    assert!(collect_subagents(&snap).is_empty());
}

#[test]
fn test_collect_subagents_only_user_bubbles() {
    let snap = ViewModelsSnapshot {
        items: im::Vector::from(vec![TuiRenderUnit::TuiUserBubble(TuiUserBubble {
            text: "hi".to_string(),
            attachments: Vec::new(),
            content_hash: 0,
            reminder: None,
            source: None,
        })]),
        generation: 0,
    };
    assert!(collect_subagents(&snap).is_empty());
}

#[test]
fn test_collect_subagents_dedup_across_committed_and_current() {
    // 同一 agent_id 出现在 items 中两次——应只保留一次
    let snap = ViewModelsSnapshot {
        items: im::Vector::from(vec![
            TuiRenderUnit::TuiSubAgentGroup(make_subagent("researcher", "Researcher")),
            TuiRenderUnit::TuiSubAgentGroup(make_subagent("researcher", "Researcher")),
        ]),
        generation: 0,
    };
    let result = collect_subagents(&snap);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].agent_id, "researcher");
}

#[test]
fn test_collect_subagents_preserves_insertion_order() {
    let snap = ViewModelsSnapshot {
        items: im::Vector::from(vec![
            TuiRenderUnit::TuiSubAgentGroup(make_subagent("alpha", "Alpha")),
            TuiRenderUnit::TuiSubAgentGroup(make_subagent("beta", "Beta")),
            TuiRenderUnit::TuiSubAgentGroup(make_subagent("gamma", "Gamma")),
        ]),
        generation: 0,
    };
    let result = collect_subagents(&snap);
    let ids: Vec<_> = result.iter().map(|s| s.agent_id.as_str()).collect();
    assert_eq!(ids, vec!["alpha", "beta", "gamma"]);
}

#[test]
fn test_collect_subagents_recurses_into_collapsed_group() {
    // TuiCollapsedGroup 内嵌 SubAgent——应被扫描到
    let collapsed = TuiCollapsedGroup {
        title: "batch".to_string(),
        count: 1,
        failed_count: 0,
        view_models: vec![TuiRenderUnit::TuiSubAgentGroup(make_subagent(
            "hidden", "Hidden",
        ))],
        fold: crate::kit::tui_render_unit::FoldState::Collapsed,
        content_hash: 0,
    };
    let snap = ViewModelsSnapshot {
        items: im::Vector::from(vec![TuiRenderUnit::TuiCollapsedGroup(collapsed)]),
        generation: 0,
    };
    let result = collect_subagents(&snap);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].agent_id, "hidden");
}

#[test]
fn test_collect_subagents_recurses_into_nested_subagent() {
    // SubAgent 内嵌 SubAgent（嵌套）——内层也应被扫描到
    let mut outer = make_subagent("outer", "Outer");
    let outer_vms: Vec<TuiRenderUnit> = vec![TuiRenderUnit::TuiSubAgentGroup(make_subagent(
        "inner", "Inner",
    ))];
    outer.view_models = im::Vector::from(outer_vms);
    let snap = ViewModelsSnapshot {
        items: im::Vector::from(vec![TuiRenderUnit::TuiSubAgentGroup(outer)]),
        generation: 0,
    };
    let result = collect_subagents(&snap);
    assert_eq!(result.len(), 2);
    assert_eq!(result[0].agent_id, "outer");
    assert_eq!(result[1].agent_id, "inner");
}

/// 只读 Info 面板（333c51d4）：Enter 与 Esc 一样关闭面板，提示必须描述真实动作。
#[test]
fn test_agent_nav_hint_describes_close_not_open() {
    let src = include_str!("agent.rs");
    // 面板主体 = 测试模块声明之前（`#[cfg(test)]` 现在也用于组件内的测试探针，
    // 故按模块声明切分，口径与迁移前一致）
    let main_src = src
        .split("#[cfg(test)]\n#[path")
        .next()
        .expect("agent module body");
    assert!(
        main_src.contains(r#"i18n::tr("panel-agent-nav-hint")"#),
        "Agent 面板底部提示须使用 Agent 专属 key"
    );
    assert!(
        main_src.contains("KeyCode::Enter => close_panel()"),
        "本用例锁定 Enter 关闭面板的动作侧；动作语义变化须同步提示与该用例"
    );
    for lang in ["en", "zh-CN"] {
        let hint = crate::i18n::LcRegistry::new(Some(lang)).tr("panel-agent-nav-hint");
        assert!(
            !hint.contains("Enter::open"),
            "{lang} 提示不得把关闭伪装成打开：{hint}"
        );
        assert!(
            hint.contains("Enter::close") || hint.contains("Enter::关闭"),
            "{lang} 提示须标明 Enter 关闭：{hint}"
        );
    }
}

// ── 面板级用例：hook 顺序、cursor 越界收敛、命中与绘制同源 ────────────────────
//
// `State<T>` 无公开构造 API，句柄只能从真实挂载捕获（visitor 模式，同 hooks/memory
// 面板测试）。命中区间取自「上一帧渲染」，故鼠标用例至少渲染到第 3 帧再投喂事件。

fn subagent_units(ids: &[&str]) -> Vec<TuiRenderUnit> {
    ids.iter()
        .map(|id| TuiRenderUnit::TuiSubAgentGroup(make_subagent(id, id)))
        .collect()
}

/// 全局 atoms 的保存/恢复——断言失败也不污染其它测试。
struct ViewModelsGuard(ViewModelsSnapshot);

impl ViewModelsGuard {
    fn install(items: Vec<TuiRenderUnit>) -> Self {
        let previous = std::mem::take(&mut *VIEW_MODELS.state().write());
        *VIEW_MODELS.state().write() = ViewModelsSnapshot {
            items: im::Vector::from(items),
            generation: 0,
        };
        Self(previous)
    }
}

impl Drop for ViewModelsGuard {
    fn drop(&mut self) {
        *VIEW_MODELS.state().write() = std::mem::take(&mut self.0);
    }
}

/// `SERVICE_SNAPSHOT` 的保存/恢复——控制 cwd 长度以构造折行。
struct ServiceInfoGuard(ServiceSnapshot);

impl ServiceInfoGuard {
    fn install(snapshot: ServiceSnapshot) -> Self {
        let previous = std::mem::take(&mut *SERVICE_SNAPSHOT.state().write());
        *SERVICE_SNAPSHOT.state().write() = snapshot;
        Self(previous)
    }
}

impl Drop for ServiceInfoGuard {
    fn drop(&mut self) {
        *SERVICE_SNAPSHOT.state().write() = std::mem::take(&mut self.0);
    }
}

/// 挂载探针的访问器：`State<T>` 无公开构造 API，句柄只能从真实挂载中捕获。
type MountVisitor = Option<Box<dyn FnMut(MountedPanel)>>;

thread_local! {
    static MOUNT_VISITOR: std::cell::RefCell<MountVisitor> =
        const { std::cell::RefCell::new(None) };
}

/// 卸载时清空 visitor：断言 panic 也不把句柄泄漏给同线程的后续用例。
struct VisitorGuard;

impl Drop for VisitorGuard {
    fn drop(&mut self) {
        MOUNT_VISITOR.with(|slot| *slot.borrow_mut() = None);
    }
}

pub(super) struct MountedPanel {
    pub(super) cursor: State<usize>,
    pub(super) sv: State<ScrollViewState>,
    pub(super) hit_spans: State<Vec<ItemSpan>>,
}

impl MountedPanel {
    /// 走真实事件入口：区域取面板实测区域，可选行数与组件同源（元信息行 + SubAgent 行）。
    fn event(&self, event: Event) -> EventResult {
        let item_count = META_ROWS + collect_subagents(&VIEW_MODELS.state().read()).len();
        handle_agent_event(
            event,
            &AgentEventCtx {
                area: Some(TEST_AREA),
                cursor: self.cursor,
                sv: self.sv,
                hit_spans: self.hit_spans,
                item_count,
            },
        )
    }
}

/// 每帧把当前句柄交给 visitor（不消费）：跨帧用例可在第 N 帧改状态、由后续帧收敛。
pub(super) fn visit_mounted_panel(panel: MountedPanel) {
    MOUNT_VISITOR.with(|slot| {
        if let Some(visitor) = slot.borrow_mut().as_mut() {
            visitor(panel);
        }
    });
}

/// 挂载并渲染 `frames` 帧，返回末帧缓冲；句柄逐帧交给 visitor。
fn mount_panel_frames(frames: usize, visitor: impl FnMut(MountedPanel) + 'static) -> Buffer {
    let _guard = VisitorGuard;
    MOUNT_VISITOR.with(|slot| {
        assert!(slot.borrow().is_none(), "上一个 visitor 未清理");
        *slot.borrow_mut() = Some(Box::new(visitor));
    });
    ratatui_kit::test_util::render_frames(element!(super::AgentPanel()), 60, 18, frames)
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

/// 在第 `act_from` 帧起投喂一次点击——命中区间取自「上一帧渲染」，第 2 帧起才有效——
/// 并断言回调确实跑过，避免渲染帧数不足让用例空转通过。
fn click_after_measured_frame(act_from: usize, mut act: impl FnMut(&MountedPanel) + 'static) {
    let acted = std::rc::Rc::new(std::cell::Cell::new(0usize));
    let counter = acted.clone();
    let mut frame = 0usize;
    mount_panel_frames(3, move |panel| {
        if frame >= act_from {
            act(&panel);
            counter.set(counter.get() + 1);
        }
        frame += 1;
    });
    assert!(
        acted.get() >= 1,
        "点击用例必须至少执行一次（act_from={act_from}）"
    );
}

fn left_down(row: u16, column: u16) -> Event {
    Event::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::NONE,
    })
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

/// cwd 元信息项的**首行**（带标签的那一行）——长值按词换行时值本身会落到第二行，
/// 用值文本定位会拿到项内第二行。
fn cwd_item_row(buf: &Buffer) -> u16 {
    rendered_row_of(buf, &i18n::tr("agent-label-cwd"))
}

/// 缓冲中首次出现 `needle` 的终端行（0 起）——即鼠标命中的绝对行号。
fn rendered_row_of(buf: &Buffer, needle: &str) -> u16 {
    for y in 0..buf.area.height {
        let mut line = String::new();
        for x in 0..buf.area.width {
            line.push_str(buf[(x, y)].symbol());
        }
        if line.contains(needle) {
            return y;
        }
    }
    panic!("渲染缓冲中找不到 {needle}");
}

/// 逐行测量必须与整段测量一致：命中区间按行累加，两者发散会让点击行号漂移。
#[test]
fn per_line_heights_sum_to_whole_content_height() {
    let lines = vec![
        Line::from("短行"),
        Line::from(""),
        Line::from("你好世界abcd"),
        Line::from("a longer line with spaces that must wrap"),
        Line::from("  ----------------------"),
    ];
    let width = 12u16;
    let sum: u16 = lines.iter().map(|line| line_height(line, width)).sum();
    assert_eq!(
        sum,
        wrapped_content_height(&lines, width),
        "逐行实测之和必须等于整段实测（ratatui 逐行换行）"
    );
    assert_eq!(line_height(&Line::from(""), width), 1, "空行占 1 行");
}

/// hook 顺序（TUI-HOOK-001）：空态与非空态在同一实例内逐帧切换，hook 序列必须稳定
/// ——某条分支改变 hook 数量时框架会按下标取到错位 hook 并 panic。
#[test]
#[serial]
fn hook_sequence_survives_subagent_branch_flip() {
    let _vm = ViewModelsGuard::install(subagent_units(&["alpha"]));
    // `frame` 是 Copy，move 进闭包的是副本——外层计数必须经 `Rc<Cell>` 观察
    let rendered = std::rc::Rc::new(std::cell::Cell::new(0usize));
    let counter = rendered.clone();
    let mut frame = 0usize;
    let buf = mount_panel_frames(4, move |_panel| {
        // 第 1 帧后清空子代理：第 2 帧起走空态分支（渲染体读 atom 早于本回调）
        if frame == 1 {
            *VIEW_MODELS.state().write() = ViewModelsSnapshot {
                items: im::Vector::new(),
                generation: 1,
            };
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
        text.contains(&i18n::tr("agent-no-subagents")),
        "空态分支必须渲染空态文案：\n{text}"
    );
}

/// 空态可见：没有子代理时渲染空态文案，而不是一片空白。
#[test]
#[serial]
fn empty_state_is_rendered() {
    let _vm = ViewModelsGuard::install(Vec::new());
    let buf = mount_panel_frames(2, |_| {});
    let text = buffer_text(&buf);
    assert!(
        text.contains(&i18n::tr("agent-no-subagents")),
        "无子代理时必须渲染空态文案：\n{text}"
    );
}

/// cursor 只能停在真实可选行上：无子代理时上下移动不越过最后一条元信息行。
#[test]
#[serial]
fn cursor_moves_only_over_selectable_rows() {
    let _vm = ViewModelsGuard::install(Vec::new());
    mount_panel_frames(2, |panel| {
        for _ in 0..20 {
            panel.event(key(KeyCode::Down));
        }
        assert_eq!(
            *panel.cursor.read(),
            META_ROWS - 1,
            "无子代理时最后可选行是最后一条元信息行"
        );
        for _ in 0..20 {
            panel.event(key(KeyCode::Up));
        }
        assert_eq!(*panel.cursor.read(), 0, "上移止于第一条元信息行");
    });
}

/// 有子代理时可选行数 = 元信息行 + 子代理行，末行不得越过子代理列表。
#[test]
#[serial]
fn cursor_reaches_last_subagent_row() {
    let _vm = ViewModelsGuard::install(subagent_units(&["alpha", "beta"]));
    mount_panel_frames(2, |panel| {
        for _ in 0..20 {
            panel.event(key(KeyCode::Down));
        }
        assert_eq!(
            *panel.cursor.read(),
            META_ROWS + 1,
            "末行是最后一个 SubAgent 行"
        );
    });
}

/// 越界收敛：列表刷新缩短可选行后，越界 cursor 由后续帧收回（渲染体不写状态）。
#[test]
#[serial]
fn cursor_converges_after_subagents_disappear() {
    let _vm = ViewModelsGuard::install(subagent_units(&["alpha", "beta", "gamma"]));
    let shrink_frames = std::rc::Rc::new(std::cell::Cell::new(0usize));
    let counter = shrink_frames.clone();
    let mut frame = 0usize;
    mount_panel_frames(4, move |panel| {
        if frame == 0 {
            for _ in 0..20 {
                panel.event(key(KeyCode::Down));
            }
            assert_eq!(
                *panel.cursor.read(),
                META_ROWS + 2,
                "先停在末尾 SubAgent 行"
            );
            // 两帧之间列表缩短（切会话 / 子代理结束）
            *VIEW_MODELS.state().write() = ViewModelsSnapshot {
                items: im::Vector::new(),
                generation: 1,
            };
        } else {
            assert!(
                *panel.cursor.read() < META_ROWS,
                "越界 cursor 必须在后续帧收敛到最后一条元信息行"
            );
            counter.set(counter.get() + 1);
        }
        frame += 1;
    });
    assert!(
        shrink_frames.get() >= 1,
        "列表缩短后必须至少再渲染一帧，否则收敛未被验证"
    );
}

/// 点击命中的是渲染出那一行的项：命中区间与绘制同源（click 只改选中，不关面板）。
#[test]
#[serial]
fn click_on_rendered_row_selects_that_item() {
    let _vm = ViewModelsGuard::install(subagent_units(&["alpha", "beta"]));

    let buf = mount_panel_frames(3, |_| {});
    let subagent_row = rendered_row_of(&buf, "beta");
    let meta_row = rendered_row_of(&buf, &i18n::tr("agent-label-provider"));

    click_after_measured_frame(2, move |panel| {
        assert_eq!(
            panel.event(left_down(subagent_row, 5)),
            EventResult::Consumed
        );
        assert_eq!(
            *panel.cursor.read(),
            META_ROWS + 1,
            "点击 SubAgent 行选中它自己"
        );

        assert_eq!(panel.event(left_down(meta_row, 5)), EventResult::Consumed);
        assert_eq!(*panel.cursor.read(), 0, "点击元信息行选中它自己");
    });
}

/// 不可选行（标题、底部提示、顶部边框）与滚动条列不改变选中。
#[test]
#[serial]
fn click_outside_selectable_rows_keeps_cursor() {
    let _vm = ViewModelsGuard::install(subagent_units(&["alpha"]));

    let buf = mount_panel_frames(3, |_| {});
    let title_row = rendered_row_of(&buf, &i18n::tr("agent-panel-title-session"));
    let hint_row = rendered_row_of(&buf, &i18n::tr("panel-agent-nav-hint"));
    let subagent_row = rendered_row_of(&buf, "alpha");

    click_after_measured_frame(2, move |panel| {
        *panel.cursor.write() = 0;
        assert_eq!(panel.event(left_down(title_row, 5)), EventResult::Consumed);
        assert_eq!(*panel.cursor.read(), 0, "标题行不可点");

        assert_eq!(panel.event(left_down(hint_row, 5)), EventResult::Consumed);
        assert_eq!(*panel.cursor.read(), 0, "底部提示行不可点");

        assert_eq!(panel.event(left_down(0, 5)), EventResult::Consumed);
        assert_eq!(*panel.cursor.read(), 0, "顶部边框行不可点");

        assert_eq!(
            panel.event(left_down(subagent_row, TEST_AREA.width - 1)),
            EventResult::Consumed
        );
        assert_eq!(*panel.cursor.read(), 0, "滚动条列不可点");
    });
}

/// 折行之后的行号不漂移：长 cwd 折成两行，其后的 SubAgent 行仍按实测区间命中。
#[test]
#[serial]
fn click_after_wrapped_row_hits_the_next_item() {
    // 元信息行前缀「' > ' + 18 列标签」= 21 列，加 45 列值 = 66 列：在真实内容宽度
    // 下折成两行——命中区间必须把这一行的额外行数算进去。
    let cwd = "x".repeat(45);
    let _snap = ServiceInfoGuard::install(ServiceSnapshot {
        cwd: cwd.clone(),
        ..ServiceSnapshot::default()
    });
    let _vm = ViewModelsGuard::install(subagent_units(&["alpha"]));

    let buf = mount_panel_frames(3, |_| {});
    let wrapped_row = cwd_item_row(&buf);
    let subagent_row = rendered_row_of(&buf, "alpha");
    assert_eq!(
        subagent_row,
        wrapped_row + 6,
        "折行 cwd 占 2 行：本项之后还有 messages、total、空行、段标题"
    );

    click_after_measured_frame(2, move |panel| {
        // 命中区间与真实渲染行同源：内容行号 = 绝对行号 - 顶部边框（1 行）
        let spans = panel.hit_spans.read().clone();
        let span_start = |item: usize| {
            spans
                .iter()
                .find(|span| span.item == item)
                .unwrap_or_else(|| panic!("第 {item} 项必须有命中区间：{spans:?}"))
                .start
        };
        assert_eq!(
            span_start(3),
            wrapped_row - 1,
            "cwd 命中区间起点必须等于渲染行（spans={spans:?}）"
        );
        assert_eq!(
            span_start(META_ROWS),
            subagent_row - 1,
            "SubAgent 命中区间起点必须等于渲染行（spans={spans:?}）"
        );

        assert_eq!(
            panel.event(left_down(wrapped_row, 5)),
            EventResult::Consumed
        );
        assert_eq!(*panel.cursor.read(), 3, "折行的 cwd 属于第 3 项");
        assert_eq!(
            panel.event(left_down(wrapped_row + 1, 5)),
            EventResult::Consumed
        );
        assert_eq!(*panel.cursor.read(), 3, "折行的第二行仍属于同一项");
        assert_eq!(
            panel.event(left_down(subagent_row, 5)),
            EventResult::Consumed
        );
        assert_eq!(*panel.cursor.read(), META_ROWS, "折行之后的行号不得偏移");
    });
}

/// 测量宽度取自 ScrollView 实测视口：整行正好放下时不得被算成折行——测量宽度差一列，
/// 其后每一项的命中行号就整体偏移一行，点击会落到别的项。
#[test]
#[serial]
fn hit_spans_follow_rendered_rows_at_wrap_boundary() {
    // 「' > ' + 18 列标签」= 21 列，加 38 列值正好 59 列 = 60 列面板扣掉滚动条列
    let cwd = "x".repeat(38);
    let _snap = ServiceInfoGuard::install(ServiceSnapshot {
        cwd: cwd.clone(),
        ..ServiceSnapshot::default()
    });
    let _vm = ViewModelsGuard::install(subagent_units(&["alpha"]));

    let buf = mount_panel_frames(3, |_| {});
    let boundary_row = rendered_row_of(&buf, &cwd);
    let subagent_row = rendered_row_of(&buf, "alpha");
    assert_eq!(
        subagent_row,
        boundary_row + 5,
        "整行正好放下时占 1 行：之后还有 messages、total、空行、段标题"
    );

    click_after_measured_frame(2, move |panel| {
        let spans = panel.hit_spans.read().clone();
        let span = |item: usize| {
            *spans
                .iter()
                .find(|span| span.item == item)
                .unwrap_or_else(|| panic!("第 {item} 项必须有命中区间：{spans:?}"))
        };
        assert_eq!(span(3).rows, 1, "边界列数下 cwd 只占 1 行：{spans:?}");
        assert_eq!(
            span(3).start,
            boundary_row - 1,
            "cwd 命中区间起点必须等于渲染行：{spans:?}"
        );
        assert_eq!(
            span(META_ROWS).start,
            subagent_row - 1,
            "SubAgent 命中区间起点必须等于渲染行：{spans:?}"
        );
        assert_eq!(
            panel.event(left_down(subagent_row, 5)),
            EventResult::Consumed
        );
        assert_eq!(*panel.cursor.read(), META_ROWS, "命中行号不得偏移一项");
    });
}

/// 滚动后点击按内容坐标命中：视口第 0 行是内容第 `offset` 行。
#[test]
#[serial]
fn click_after_scroll_uses_the_scroll_offset() {
    let _snap = ServiceInfoGuard::install(ServiceSnapshot {
        cwd: "x".repeat(38),
        ..ServiceSnapshot::default()
    });
    let _vm = ViewModelsGuard::install(subagent_units(&["alpha"]));

    let buf = mount_panel_frames(3, |_| {});
    let wrapped_row = cwd_item_row(&buf);

    click_after_measured_frame(2, move |panel| {
        // 视口下移一行：同一内容行渲染在上一行
        panel.sv.write().set_offset(Position::new(0, 1));
        assert_eq!(
            panel.event(left_down(wrapped_row - 1, 5)),
            EventResult::Consumed
        );
        assert_eq!(*panel.cursor.read(), 3, "滚动偏移必须加回内容行号");
    });
}
