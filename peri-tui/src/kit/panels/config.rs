//! ratatui-kit ConfigPanel component.
//!
//! H1a（Iteration 14）：从 PERI_CONFIG_HANDLE 读取真实 PeriConfig，操作时
//! write + 调用 config::save_effective 持久化到当前生效层（全局或工作区，
//! 路径决策由 ConfigSource 加载时确定）。permission_mode
//! 经 session/set_mode 修改当前会话运行权限，不写宿主持久配置。
//!
//! 职责划分：本文件是组件外壳（hooks、事件注册、渲染窗口）；行模型与行显示在
//! [`rows`]，写入与保存边界在 [`actions`]。两边都以行模型的下标为身份。

mod actions;
mod rows;

use self::actions::*;
use self::rows::*;

use crate::app::panel_types::PanelKind;
use crate::i18n;
use crate::kit::atoms::{LANG_VERSION, SERVICE_SNAPSHOT};
use crate::kit::list_nav::{next_selection, previous_selection};
use crate::kit::panel_mouse::{AreaTracker, hit_item, is_scrollbar_column};
use crate::kit::panels::shared::list::{ListMetrics, ListViewport, content_rows};
use peri_theme::atoms::THEME_ATOM;
use ratatui_kit::{
    crossterm::event::{Event, KeyCode, KeyEventKind, MouseButton, MouseEventKind},
    prelude::*,
    ratatui::{
        layout::{Constraint, Rect},
        style::{Style, Stylize},
        text::{Line, Span},
        widgets::Paragraph,
    },
};

/// 列表行度量：header 2 行（标题、空行）+ 每行 1 行 + footer 2 行（空行、导航提示）。
///
/// 渲染与鼠标命中都取自本度量推导出的同一 `ListViewport`；此前命中布局用
/// `visible_items = 整表行数, scroll_start = 0`，与渲染各算一份。
const LIST_METRICS: ListMetrics = ListMetrics::new(2, 1, 2);

#[component]
pub fn ConfigPanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    // hooks 全部无条件前置（TUI-HOOK-001）：这里到事件注册之间不得出现条件分支或
    // 提前 return——渲染路径变化时 hook 数量与类型必须一致。
    hooks.use_atom(&SERVICE_SNAPSHOT);
    let theme_def = hooks.use_atom(&THEME_ATOM);
    let cursor = hooks.use_state(|| 0usize);
    // 外部滚动状态——面板滚轮仲裁（panel_scroll.rs）驱动，统一 3 行/格 + 节流
    let sv = hooks.use_state(ScrollViewState::default);
    // bump：每次操作后递增，强制重渲染（PERI_CONFIG_HANDLE 是 RwLock 非 atom，
    // 写入不会自动触发 ratatui-kit 重渲染，需要手动 bump）
    let bump = hooks.use_state(|| 0u32);
    let _lang_ver = hooks.use_atom(&LANG_VERSION);

    // 面板绘制区域（上一帧）——鼠标行号反推与滚轮仲裁注册共用这一份区域。
    let area = hooks.use_hook(AreaTracker::new).rect;
    let rows = config_rows();
    let row_count = rows.len();

    // 列表视口：绘制（window）与鼠标命中（hit_layout）共用同一实例，可见行数由
    // 面板内容区行数推导——终端/面板高度变化后两者一起变化。
    let (_term_w, term_h) = hooks.use_terminal_size();
    let viewport = ListViewport::for_content_rows(
        content_rows(area, term_h),
        LIST_METRICS,
        row_count,
        *cursor.read(),
    );

    hooks.use_event_handler_with_options(
        EventScope::Current,
        EventPriority::Normal,
        EventOptions { hit_test: true },
        move |event| {
            handle_config_event(
                event,
                &ConfigEventCtx {
                    area,
                    cursor,
                    bump,
                    viewport,
                    row_count,
                },
            )
        },
    );

    #[cfg(test)]
    tests::visit_mounted_panel(tests::MountedPanel { cursor, bump });

    // 读取 bump 强制 ratatui-kit 把这个值当作依赖（无此 read 调用则不会重渲染）
    let _ = *bump.read();

    // ---- Render ----（只读状态：TUI-RENDER-001，写状态只在事件与 effect 边界）
    let sel = *cursor.read();
    let mut lines: Vec<Line<'_>> = Vec::new();

    lines.push(Line::from(vec![Span::styled(
        format!("  {}", i18n::tr("config-panel-title")),
        Style::new()
            .fg(theme_def.read().semantic.text.muted)
            .italic(),
    )]));
    lines.push(Line::from(""));

    // 只渲染视口窗口内的行——面板矮于行数时窗口跟随选中项滚动，
    // 鼠标命中（`hit_layout`）看到的是同一窗口。
    for (i, row) in viewport.window(&rows) {
        lines.push(build_row_line(*row, i == sel, &theme_def.read()));
    }

    // Footer hints — use single i18n key for consistency
    lines.push(Line::from(""));
    lines
        .push(Line::from(i18n::tr("panel-config-nav-hint")).fg(theme_def.read().semantic.text.dim));

    let content = Paragraph::new(ratatui::text::Text::from(lines));

    // 面板滚轮仲裁注册（每帧覆盖写入；区域与列表命中同源，均取 AreaTracker 的上一帧
    // 区域，首帧未测量时为 0 尺寸，下一帧起生效——首帧滚轮不经本面板仲裁）
    crate::kit::panel_scroll::register_panel_scroll(
        PanelKind::Config,
        area.unwrap_or_default(),
        sv,
    );

    panel_shell!(PanelKind::Config, {
        ScrollView(
            scrollbars: crate::kit::panel_registry::clean_scrollbars(),
            state: Some(sv),
            width: Constraint::Fill(1),
            height: Constraint::Fill(1),
        ) {
            Text(text: content)
        }
    })
}

/// 面板事件处理——判定集中在这里、组件只做注册，测试可经 `tests::mount_panel`
/// 捕获真实句柄后直接投喂事件（`State<T>` 无公开构造 API）。
///
/// 鼠标命中与绘制共用 `ctx.viewport`（`hit_layout()` / `window()` 同源）。
fn handle_config_event(event: Event, ctx: &ConfigEventCtx) -> EventResult {
    // 鼠标：区域内左键点击 = 选中该项并执行 Enter 动作（click as enter）
    if let Event::Mouse(mouse) = event {
        if let Some(area) = ctx.area
            && !is_scrollbar_column(&mouse, area)
            && let Some(idx) = hit_item(&mouse, area, ctx.viewport.hit_layout())
        {
            *ctx.cursor.write() = idx;
            activate_row(idx, true);
            *ctx.bump.write() += 1;
            return EventResult::Consumed;
        }
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
    let sel = *ctx.cursor.read();

    match key.code {
        KeyCode::Esc => {
            close_panel();
        }
        KeyCode::Up => {
            *ctx.cursor.write() = previous_selection(sel);
        }
        KeyCode::Down => {
            *ctx.cursor.write() = next_selection(sel, ctx.row_count);
        }
        KeyCode::Char(' ') | KeyCode::Enter => {
            activate_row(sel, true);
            *ctx.bump.write() += 1;
        }
        KeyCode::Left => {
            activate_row(sel, false);
            *ctx.bump.write() += 1;
        }
        KeyCode::Right => {
            activate_row(sel, true);
            *ctx.bump.write() += 1;
        }
        _ => {}
    }
    EventResult::Consumed
}

/// 事件处理上下文：本帧视口 + 面板句柄 + 行模型长度。
///
/// `State<T>` 无公开构造 API，只能从组件挂载取得；集中成一个结构体，事件入口
/// 签名不再随状态/视口增长（渲染、鼠标命中与滚轮仲裁必须共用同一份区域与视口）。
struct ConfigEventCtx {
    area: Option<Rect>,
    cursor: State<usize>,
    bump: State<u32>,
    viewport: ListViewport,
    /// 行模型长度（行模型由静态表派生，长度即键盘导航的上界）
    row_count: usize,
}

fn close_panel() {
    // I19-A: 弹栈而非清空整个栈，避免同时打开多个不同组面板时关闭一个会全部关闭
    crate::kit::panel_registry::close_active_panel();
}

#[cfg(test)]
#[path = "config_test.rs"]
mod tests;

#[cfg(test)]
#[path = "config_betas_test.rs"]
mod beta_tests;
