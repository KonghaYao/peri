use std::collections::HashMap;

use super::{
    THEME_LIST, ThemeCatalogTabs, ThemePanel, build_preview, classify_theme_catalog,
    list_header_rows, refresh_theme_catalog_after_download, resolve_selection,
};
use peri_theme::atoms::{PALETTE_ATOM, PERI_COLORS_ATOM, THEME_ATOM};
use peri_theme::theme::ThemeMode;
use ratatui_kit::crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui_kit::prelude::{EventResult, State, element};
use ratatui_kit::ratatui::buffer::Buffer;
use ratatui_kit::ratatui::layout::Rect;
use ratatui_kit::ratatui::text::Line;
use serial_test::serial;

/// [回归测试] 下载成功后必须以重新扫描到的 catalog 覆盖旧快照，并按最新列表重新分类，
/// 避免已打开的 ThemePanel 继续显示下载前的主题列表。
#[test]
fn downloaded_catalog_replaces_snapshot_and_reclassifies() {
    let mut catalog = vec!["stale-dark".to_string()];

    refresh_theme_catalog_after_download(&mut catalog, 1, || {
        vec![
            "downloaded-dark".to_string(),
            "downloaded-light".to_string(),
        ]
    });

    assert_eq!(
        catalog,
        vec!["downloaded-dark", "downloaded-light"],
        "下载后的 catalog 应覆盖旧快照"
    );

    let modes = HashMap::from([
        ("downloaded-dark", ThemeMode::Dark),
        ("downloaded-light", ThemeMode::Light),
    ]);
    let (dark, light) = classify_theme_catalog(&catalog, |name| modes.get(name).copied());

    assert_eq!(dark, vec!["downloaded-dark"], "新增深色主题应可见");
    assert_eq!(light, vec!["downloaded-light"], "新增浅色主题应可见");
}

/// [回归测试] 当本次下载没有成功写入时，不应刷新主题目录快照。
#[test]
fn failed_download_does_not_rescan_or_replace_catalog() {
    let mut catalog = vec!["existing-theme".to_string()];
    let mut scan_called = false;

    refresh_theme_catalog_after_download(&mut catalog, 0, || {
        scan_called = true;
        vec!["unexpected-theme".to_string()]
    });

    assert!(!scan_called, "没有成功写入时不应重新扫描主题目录");
    assert_eq!(catalog, vec!["existing-theme"]);
}

// ── 面板级用例：hook 顺序、渲染只读、命中/绘制同源、选中身份稳定 ──────────────
//
// `State<T>` 无公开构造 API，句柄只能从真实挂载捕获（visitor 模式，同 memory/mcp
// 面板测试）。主题名取内置主题与别名（`dark` / `light` 是 `peri-dark` /
// `peri-light` 的别名，均可加载），分类结果因此可预期。

/// 测试面板区域：整屏 80×24（组件直接渲染进缓冲，组件区域即整屏）。
const TEST_AREA: Rect = Rect {
    x: 0,
    y: 0,
    width: 80,
    height: 24,
};

fn catalog(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| (*name).to_string()).collect()
}

/// 全局主题/目录 atom 的保存与恢复——断言失败也不污染其它测试。
struct ThemeAtomsGuard {
    theme: std::sync::Arc<peri_theme::theme::ThemeDefinition>,
    palette: ratatui_kit::prelude::Palette,
    peri: std::sync::Arc<peri_theme::peri_colors::PeriColors>,
    catalog: Vec<String>,
}

impl ThemeAtomsGuard {
    /// 安装目录快照与当前主题。
    ///
    /// 面板挂载首帧会切到「持久化主题」（测试进程无 `TUI_CONFIG_HANDLE`，取默认
    /// `peri-dark`），故当前主题名用 `peri-dark` 时挂载不额外改写 atom。
    fn install(names: &[&str], current_theme: &str) -> Self {
        let guard = Self {
            theme: THEME_ATOM.state().read().clone(),
            palette: *PALETTE_ATOM.state().read(),
            peri: PERI_COLORS_ATOM.state().read().clone(),
            catalog: THEME_LIST.state().read().clone(),
        };
        *THEME_LIST.state().write() = catalog(names);
        THEME_ATOM
            .state()
            .set(peri_theme::loader::load_theme(current_theme).expect("测试主题必须可加载"));
        guard
    }
}

impl Drop for ThemeAtomsGuard {
    fn drop(&mut self) {
        THEME_ATOM.state().set(self.theme.clone());
        PALETTE_ATOM.state().set(self.palette);
        PERI_COLORS_ATOM.state().set(self.peri.clone());
        *THEME_LIST.state().write() = std::mem::take(&mut self.catalog);
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
    pub(super) tab: State<u8>,
    pub(super) selected_theme: State<String>,
    pub(super) last_index: State<usize>,
    pub(super) preview_cache: State<HashMap<String, Vec<Line<'static>>>>,
    /// 挂载时捕获的原始主题名（Esc 回滚用）
    pub(super) original_theme: State<String>,
}

impl MountedPanel {
    /// 走真实事件入口：本帧列表快照、选中落点与 header 行数按组件同一推导
    /// （命中布局与绘制共用 `list_header_rows`）。
    ///
    /// 快照先取出再投喂事件——`State::read()` 的读守卫若跨过事件调用，事件里的
    /// `write()` 会在同一线程自锁。
    fn event(&self, event: Event, area: Rect) -> EventResult {
        let themes = ThemeCatalogTabs::of(&THEME_LIST.state().read())
            .list(*self.tab.read())
            .to_vec();
        let selected_theme = self.selected_theme.read().clone();
        let selection = resolve_selection(&themes, &selected_theme, *self.last_index.read());
        let original_theme = self.original_theme.read().clone();
        super::handle_theme_event(
            event,
            &super::ThemeEventCtx {
                area: Some(area),
                tab: self.tab,
                selected_theme: self.selected_theme,
                header_rows: list_header_rows(build_preview(&selected_theme).len()),
                scroll_start: crate::kit::list_nav::scroll_start_for_selected(
                    selection.index,
                    themes.len(),
                    super::VISIBLE_ITEMS,
                ),
                selected_index: selection.index,
                themes,
                original_theme,
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

/// 挂载并渲染 `frames` 帧，返回末帧缓冲。
fn mount_panel_at(frames: usize, visitor: impl FnMut(MountedPanel) + 'static) -> Buffer {
    let _guard = VisitorGuard;
    MOUNT_VISITOR.with(|slot| {
        assert!(slot.borrow().is_none(), "上一个 visitor 未清理");
        *slot.borrow_mut() = Some(Box::new(visitor));
    });
    ratatui_kit::test_util::render_frames(
        element!(ThemePanel()),
        TEST_AREA.width,
        TEST_AREA.height,
        frames,
    )
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
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

/// 列表项行号：footer 提示行紧跟在最后一项之后（列表每项 1 行），据此反推。
fn item_row_from_footer(buf: &Buffer, items_below: u16) -> u16 {
    rendered_row_of(buf, "Ctrl+T") - items_below
}

/// 命中与绘制同源：绘制出的 header 行数必须等于命中布局（`hit_item`）使用的行数，
/// 且点击「画着第 k 项」的那一行必须选中第 k 项。
///
/// 旧实现两处各自手算：命中布局 `3 + preview_len + 1` 少算了预览标题行，点击会落到
/// 下一项（列表末尾则落空）。
#[test]
#[serial]
fn click_on_rendered_item_row_selects_that_item() {
    let _atoms = ThemeAtomsGuard::install(&["peri-dark", "dark", "light"], "peri-dark");

    // 第一遍：只渲染，行号全部取自缓冲（不复用生产公式）
    let buf = ratatui_kit::test_util::render_frames(
        element!(ThemePanel()),
        TEST_AREA.width,
        TEST_AREA.height,
        2,
    );
    let second_item_row = item_row_from_footer(&buf, 1);
    let drawn_header_rows = (second_item_row - 1 - (TEST_AREA.y + 1)) as usize;
    assert_eq!(
        drawn_header_rows,
        list_header_rows(build_preview("peri-dark").len()) as usize,
        "命中布局的 header 行数必须等于绘制出的 header 行数"
    );

    // 第二遍：挂载后点击第二项绘制所在行，应选中第二项（`dark`）
    let (tx, rx) = std::sync::mpsc::channel();
    let _ = mount_panel_at(2, move |panel| {
        if *panel.selected_theme.read() == "peri-dark" {
            let result = panel.event(left_down(second_item_row, 5), TEST_AREA);
            tx.send((result, panel.selected_theme.read().clone()))
                .expect("测试结果通道仍打开");
        }
    });

    let (result, selected) = rx.try_recv().expect("点击必须走真实事件入口");
    assert_eq!(result, EventResult::Consumed);
    assert_eq!(
        selected, "dark",
        "点击画着第二项的那一行必须选中第二项（header 差一行会落到下一项）"
    );
}

/// 预览为空时 header 仍按渲染的一行 `(no preview)` 计入——命中与绘制同源。
#[test]
fn empty_preview_counts_one_header_row() {
    let with_preview = list_header_rows(4);
    assert_eq!(
        with_preview,
        super::LIST_HEADER_FIXED_ROWS as u16 + 4,
        "有预览时 header = 固定 5 行 + 预览正文行数"
    );
    assert_eq!(
        list_header_rows(0),
        super::LIST_HEADER_FIXED_ROWS as u16 + 1,
        "预览为空时渲染一行 (no preview)，命中布局必须同样计入"
    );
}

/// 身份不随刷新漂移：目录刷新改变顺序后，选中身份仍指向同一主题（下标随之变化）。
#[test]
#[serial]
fn selection_identity_survives_catalog_refresh() {
    let _atoms = ThemeAtomsGuard::install(&["peri-dark", "dark", "light"], "peri-dark");

    let mut frame = 0usize;
    let buf = mount_panel_at(4, move |panel| {
        if frame == 0 {
            // 选中第二项（`dark`）：旧实现只记下标
            *panel.selected_theme.write() = "dark".to_string();
        }
        if frame == 1 {
            // 刷新目录：顺序反转，下标 1 从 `dark` 变成 `peri-dark`
            *THEME_LIST.state().write() = catalog(&["dark", "peri-dark", "light"]);
        }
        frame += 1;
    });

    let text = buffer_text(&buf);
    assert!(
        text.contains("> dark"),
        "刷新后高亮必须仍在同一主题（身份 `dark`）上：\n{text}"
    );
}

/// 选中主题从目录消失（下载后被删除/改名）时收敛到相邻项且不越界。
#[test]
#[serial]
fn selection_converges_to_neighbour_when_theme_disappears() {
    let _atoms = ThemeAtomsGuard::install(&["peri-dark", "dark", "light"], "peri-dark");

    let mut frame = 0usize;
    let (tx, rx) = std::sync::mpsc::channel();
    let buf = mount_panel_at(4, move |panel| {
        if frame == 0 {
            *panel.selected_theme.write() = "dark".to_string(); // 下标 1
        }
        if frame == 1 {
            // `dark` 从目录消失：列表只剩一项，收敛位必须 clamp 进列表
            *THEME_LIST.state().write() = catalog(&["peri-dark", "light"]);
        }
        if frame == 3 {
            tx.send((
                panel.selected_theme.read().clone(),
                *panel.last_index.read(),
                *panel.tab.read(),
            ))
            .expect("测试结果通道仍打开");
        }
        frame += 1;
    });

    let (selected, last_index, tab) = rx.try_recv().expect("末帧必须上报状态");
    assert_eq!(
        selected, "peri-dark",
        "身份消失时收敛到相邻项（下标 clamp 到列表内），而不是越界或清空"
    );
    assert_eq!(last_index, 0, "落点必须收敛进列表");
    assert_eq!(tab, 0, "收敛不改变 tab");
    let text = buffer_text(&buf);
    assert!(text.contains("> peri-dark"), "渲染必须跟着收敛：\n{text}");
}

/// tab 切换（旧实现里 hook 之后的条件块路径）后句柄仍指向各自槽位，选中身份一致：
/// 新 tab 不含当前主题时收敛到该 tab 的相邻项。
#[test]
#[serial]
fn tab_switch_converges_selection_and_keeps_handles_coherent() {
    let _atoms =
        ThemeAtomsGuard::install(&["peri-dark", "dark", "light", "peri-light"], "peri-dark");

    let light_items = catalog(&["light", "peri-light"]);
    let mut frame = 0usize;
    let (tx, rx) = std::sync::mpsc::channel();
    mount_panel_at(6, move |panel| {
        if frame == 0 {
            // `peri-dark` 不在 light tab 列表中 → 收敛到 light 列表的相邻项
            let _ = panel.event(key(KeyCode::Tab), TEST_AREA);
        }
        if frame == 2 {
            tx.send((
                *panel.tab.read(),
                panel.selected_theme.read().clone(),
                panel.preview_cache.read().len(),
            ))
            .expect("测试结果通道仍打开");
        }
        frame += 1;
    });

    let (tab, selected, cached_previews) = rx.try_recv().expect("切 tab 后必须上报状态");
    assert_eq!(tab, 1, "Tab 必须切到 light tab（状态槽位未错位）");
    assert!(
        light_items.contains(&selected),
        "收敛后的选中身份必须属于 light 列表，实际 {selected}"
    );
    assert!(
        cached_previews > 0,
        "预览缓存由 effect 边界写入（渲染只读），必须已缓存"
    );
}

/// 渲染只读：连续渲染帧不改变选中身份与 tab（TUI-RENDER-001）。
#[test]
#[serial]
fn rendering_frames_leaves_selection_and_tab_untouched() {
    let _atoms = ThemeAtomsGuard::install(&["peri-dark", "dark", "light"], "peri-dark");

    let mut frame = 0usize;
    let (tx, rx) = std::sync::mpsc::channel();
    mount_panel_at(4, move |panel| {
        if frame == 0 {
            *panel.selected_theme.write() = "dark".to_string();
        }
        if frame == 3 {
            tx.send((
                panel.selected_theme.read().clone(),
                *panel.tab.read(),
                *panel.last_index.read(),
            ))
            .expect("测试结果通道仍打开");
        }
        frame += 1;
    });

    let (selected, tab, last_index) = rx.try_recv().expect("末帧必须上报状态");
    assert_eq!(selected, "dark", "渲染帧不得改写选中身份");
    assert_eq!(tab, 0, "渲染帧不得改写 tab");
    assert_eq!(last_index, 1, "渲染帧只记录落点，不改写选中");
}

/// TUI-RENDER-001：渲染段不得写状态。
///
/// 挂载帧区分不了「渲染体写」与「effect 写」（两者在同一帧内生效），故与
/// `hooks_test` 同法对源码结构断言：渲染段标记到 `panel_shell!` 之间不得出现
/// 状态写入（用户可见行为由 `rendering_frames_leaves_selection_and_tab_untouched`
/// 与 `preview_follows_selection` 覆盖）。
#[test]
fn render_section_writes_no_state() {
    let src = include_str!("theme.rs");
    let render = src
        .split("// ── 渲染段（只读")
        .nth(1)
        .expect("渲染段标记存在")
        .split("panel_shell!")
        .next()
        .expect("渲染段以 panel_shell 结束");
    for forbidden in [".write()", ".write_no_update()", ".set(", ".set_no_update("] {
        assert!(
            !render.contains(forbidden),
            "渲染段不得写状态（TUI-RENDER-001）：发现 {forbidden}"
        );
    }
}

/// 预览跟随选中项：Up/Down 只换预览不落盘，Enter 才落盘（时机不变）。
#[test]
#[serial]
fn preview_follows_selection_on_navigation() {
    let _atoms = ThemeAtomsGuard::install(&["peri-dark", "dark", "light"], "peri-dark");

    let mut frame = 0usize;
    let (tx, rx) = std::sync::mpsc::channel();
    mount_panel_at(3, move |panel| {
        if frame == 0 {
            let result = panel.event(key(KeyCode::Down), TEST_AREA);
            tx.send((
                result,
                panel.selected_theme.read().clone(),
                *panel.last_index.read(),
            ))
            .expect("测试结果通道仍打开");
        }
        frame += 1;
    });

    let (result, selected, _) = rx.try_recv().expect("Down 必须走真实事件入口");
    assert_eq!(result, EventResult::Consumed);
    assert_eq!(
        selected, "dark",
        "Down 后选中身份落到下一项（预览随之切换）"
    );
}

/// Esc 的既有语义不变：先恢复打开面板时的主题（预览回滚），再返回 `Ignored`
/// 让容器关闭面板；选中身份不被 Esc 改写（面板随后关闭）。
#[test]
#[serial]
fn esc_restores_original_theme_and_defers_close() {
    let _atoms = ThemeAtomsGuard::install(&["peri-dark", "dark", "light"], "peri-dark");

    let mut frame = 0usize;
    let (tx, rx) = std::sync::mpsc::channel();
    mount_panel_at(2, move |panel| {
        if frame == 0 {
            // 模拟预览：面板把全局颜色切到另一主题
            super::switch_theme_atoms("peri-light");
            let previewed = THEME_ATOM.state().read().name.to_string();
            let original = panel.original_theme.read().clone();
            let selected = panel.selected_theme.read().clone();
            let result = panel.event(key(KeyCode::Esc), TEST_AREA);
            tx.send((
                previewed,
                original,
                selected,
                result,
                THEME_ATOM.state().read().name.to_string(),
                panel.selected_theme.read().clone(),
            ))
            .expect("测试结果通道仍打开");
        }
        frame += 1;
    });

    let (previewed, original, selected, result, restored, selected_after) =
        rx.try_recv().expect("Esc 必须走真实事件入口");
    assert_eq!(original, "peri-dark", "挂载时记住的主题名");
    assert_eq!(previewed, "peri-light", "预览已切到另一主题");
    assert_eq!(
        result,
        EventResult::Ignored,
        "Esc 不由面板消费——容器据此关闭面板"
    );
    assert_eq!(restored, original, "Esc 必须把预览回滚到原始主题");
    assert_eq!(selected_after, selected, "Esc 不改写选中身份");
}
