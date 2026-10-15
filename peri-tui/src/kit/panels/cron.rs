//! ratatui-kit CronPanel component.
//!
//! Session-scoped Cron projection and ACP operations.

use ratatui_kit::{
    crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind},
    prelude::*,
    ratatui::{
        layout::{Constraint, Rect},
        style::{Style, Stylize},
        text::{Line, Span},
        widgets::Paragraph,
    },
};

use crate::app::panel_types::PanelKind;
use crate::i18n;
use crate::kit::atoms::{
    CRON_ACTION_ERROR, CRON_JOBS, CronJobSummary, LANG_VERSION, NOTIFICATION, Notification,
    SERVICE_PROJECTION_ERROR,
};
use crate::kit::list_nav::{next_selection, previous_selection};
use crate::kit::panel_mouse::{AreaTracker, hit_item, is_scrollbar_column};
use crate::kit::panels::shared::list::{ListMetrics, ListViewport, content_rows};
use fluent_bundle::FluentValue;
use peri_theme::atoms::THEME_ATOM;

mod actions;

/// cron 动作方法名（host 侧 ACP 方法）。
const CRON_TOGGLE_METHOD: &str = "cron/toggle";
const CRON_REMOVE_METHOD: &str = "cron/remove";

/// 每个任务占据的行数：标签行、prompt 摘要行、next fire 行（缺失时占位空行）。
const ITEM_ROWS: u16 = 3;

/// 列表行度量：header = 统计行（列表非空才渲染）+ 提示/确认行 + 错误行，
/// 其后每项 [`ITEM_ROWS`] 行，无 footer（空列表的两行提示文案落在列表区域，
/// 但 `item_count == 0` 时不参与命中）。
///
/// 绘制与鼠标命中都取本函数的结果，空列表少一行统计行不会让两者错位。
fn list_metrics(jobs_empty: bool) -> ListMetrics {
    ListMetrics::new(if jobs_empty { 2 } else { 3 }, ITEM_ROWS, 0)
}

#[component]
pub fn CronPanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme_def = hooks.use_atom(&THEME_ATOM);
    let selected = hooks.use_state(|| 0usize);
    // 待删任务的稳定身份（job.id）；None = 不在确认模式。
    // 进入确认时固定 id，确认执行按 id 定位——确认期间列表刷新/重排不会把删除落到别的任务。
    let confirm_delete = hooks.use_state(|| Option::<String>::None);
    // 外部滚动状态——面板滚轮仲裁（panel_scroll.rs）驱动，统一 3 行/格 + 节流
    let sv = hooks.use_state(ScrollViewState::default);

    // S6c: 订阅 CRON_JOBS atom——后台 service_snapshot 2s 派生一次
    let jobs_store = hooks.use_atom(&CRON_JOBS);
    let jobs: Vec<CronJobSummary> = jobs_store.read().clone();
    let _ = jobs_store; // StoreState 是 Copy，无需显式 drop
    let _ = hooks.use_atom(&LANG_VERSION);
    let projection_error = hooks.use_atom(&SERVICE_PROJECTION_ERROR);
    let action_error = hooks.use_atom(&CRON_ACTION_ERROR);

    // 面板绘制区域（上一帧）——鼠标行号反推与滚轮仲裁注册共用这一份区域。
    let area = hooks.use_hook(AreaTracker::new).rect;
    // 列表视口：绘制（window）与鼠标命中（hit_layout）共用同一实例，可见项数由面板
    // 内容区行数推导——面板高度随终端变化时两者一起变化，不再写死可见项常量。
    let (_term_w, term_h) = hooks.use_terminal_size();
    let viewport = ListViewport::for_content_rows(
        content_rows(area, term_h),
        list_metrics(jobs.is_empty()),
        jobs.len(),
        *selected.read(),
    );

    // 事件闭包另持一份列表副本（与渲染端共用同一 atom 快照）
    let jobs_for_events = jobs.clone();
    hooks.use_event_handler_with_options(
        EventScope::Current,
        EventPriority::Normal,
        EventOptions { hit_test: true },
        move |event| {
            handle_cron_event(
                event,
                &CronEventCtx {
                    area,
                    selected,
                    confirm_delete,
                    viewport,
                    jobs: &jobs_for_events,
                },
            )
        },
    );

    #[cfg(test)]
    tests::visit_mounted_panel(tests::MountedPanel {
        selected,
        confirm_delete,
    });

    let sel = *selected.read();
    let is_confirming = confirm_delete.read().is_some();

    let enabled_count = jobs.iter().filter(|e| e.enabled).count();
    let mut lines: Vec<Line<'_>> = Vec::new();
    let errors = [projection_error.read().clone(), action_error.read().clone()];

    // Stats line
    if !jobs.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            i18n::tr_args(
                "panel-cron-stats",
                &[
                    (
                        "configured".to_string(),
                        FluentValue::from(jobs.len() as i64),
                    ),
                    (
                        "enabled".to_string(),
                        FluentValue::from(enabled_count as i64),
                    ),
                ],
            ),
            Style::new()
                .fg(theme_def.read().semantic.text.primary)
                .bold(),
        )]));
    }

    // Hint / confirm-delete line
    if is_confirming {
        lines.push(Line::from(vec![Span::styled(
            i18n::tr("panel-cron-confirm-hint"),
            Style::new().fg(theme_def.read().semantic.status.warning),
        )]));
    } else {
        lines.push(Line::from(vec![Span::styled(
            i18n::tr("panel-cron-nav-hint"),
            Style::new().fg(theme_def.read().semantic.text.muted),
        )]));
    }
    lines.push(Line::from(Span::styled(
        errors.into_iter().flatten().collect::<Vec<_>>().join("; "),
        Style::new().fg(theme_def.read().semantic.status.warning),
    )));

    // Task list
    if jobs.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            i18n::tr("panel-cron-empty"),
            Style::new().fg(theme_def.read().semantic.text.muted),
        )]));
        lines.push(Line::from(vec![Span::styled(
            i18n::tr("panel-cron-empty-hint"),
            Style::new().fg(theme_def.read().semantic.text.muted),
        )]));
    } else {
        for (i, entry) in viewport.window(&jobs) {
            let is_selected = i == sel;
            let cursor = if is_selected { ">" } else { " " };
            let name_style = if is_selected {
                Style::new()
                    .fg(theme_def.read().component.panel.title)
                    .bold()
            } else {
                Style::new().fg(theme_def.read().semantic.text.primary)
            };
            let enabled_label = if entry.enabled {
                i18n::tr("panel-cron-status-on")
            } else {
                i18n::tr("panel-cron-status-off")
            };
            let enabled_style = if entry.enabled {
                Style::new().fg(theme_def.read().semantic.status.success)
            } else {
                Style::new().fg(theme_def.read().semantic.text.muted)
            };

            // Label line: cursor + num + schedule + [ON/OFF]
            lines.push(Line::from(vec![
                Span::styled(
                    format!(" {} {}. {} ", cursor, i + 1, entry.expression),
                    name_style,
                ),
                Span::styled(
                    i18n::tr_args(
                        "panel-cron-status-format",
                        &[("status".to_string(), FluentValue::from(enabled_label))],
                    ),
                    enabled_style,
                ),
            ]));

            // Detail line: prompt summary (indented)
            let prompt_summary: String = entry
                .prompt
                .chars()
                .take(50)
                .chain(if entry.prompt.chars().count() > 50 {
                    Some('…')
                } else {
                    None
                })
                .collect();
            lines.push(Line::from(vec![Span::styled(
                format!("     {}", prompt_summary),
                Style::new().fg(theme_def.read().semantic.text.primary),
            )]));

            // Next fire timestamp (if available)
            if let Some(next) = entry.next_fire {
                let ts = next.format("%Y-%m-%d %H:%M").to_string();
                lines.push(Line::from(vec![Span::styled(
                    i18n::tr_args(
                        "panel-cron-next-fire",
                        &[("time".to_string(), FluentValue::from(ts))],
                    ),
                    Style::new().fg(theme_def.read().semantic.text.muted),
                )]));
            } else {
                // next_fire 缺失时占位空行，保证每项固定 3 行（视口计算依赖）
                lines.push(Line::from(""));
            }
        }
    }

    let content = if lines.is_empty() {
        Paragraph::new(
            Line::from(i18n::tr("common-empty")).fg(theme_def.read().semantic.text.muted),
        )
    } else {
        Paragraph::new(ratatui::text::Text::from(lines))
    };

    // 面板滚轮仲裁注册（每帧覆盖写入；区域与列表命中同源——都由 AreaTracker 的
    // 上一帧区域提供，首帧尚未测量时为 0 尺寸，与 `use_previous_size()` 的首帧
    // `Rect::default()` 等价，但少了第二份区域来源）。
    crate::kit::panel_scroll::register_panel_scroll(PanelKind::Cron, area.unwrap_or_default(), sv);

    panel_shell!(PanelKind::Cron, {
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

/// H1g：cron 动作出口（toggle/remove）。service_snapshot 下次 tick 自动刷新 UI（≤2s）。
fn cron_action(method: &'static str, id: &str) {
    actions::dispatch(method, id);
}

/// 提示「确认的删除目标已消失」——走跨面板的确认目标守卫文案。
///
/// 确认期间列表刷新使目标不在列表里时，本次删除不执行（删除不可逆，不回退为按下标
/// 取「现在这一项」）；这是一次被放弃的操作，必须让用户看到，不能只留日志。
fn notify_delete_target_missing(job_id: &str) {
    *NOTIFICATION.state().write() = Some(Notification {
        message: i18n::tr_args(
            "app-confirm-target-missing",
            &[("target".to_string(), FluentValue::from(job_id))],
        ),
        until: peri_time::monotonic_now() + std::time::Duration::from_secs(2),
    });
}

/// 面板事件处理——判定集中在这里，组件只做注册；测试可经挂载探针取真实句柄后直接投喂。
fn handle_cron_event(event: Event, ctx: &CronEventCtx<'_>) -> EventResult {
    handle_cron_event_with(event, ctx, cron_action)
}

/// 事件判定本体：动作出口注入（生产路径 = [`cron_action`]，测试 = 捕获闭包），
/// 使「对哪个 id 做什么」可断言而不触发真实 RPC（仿 `config::toggle_beta_flag_with`）。
fn handle_cron_event_with(
    event: Event,
    ctx: &CronEventCtx<'_>,
    action: impl Fn(&'static str, &str),
) -> EventResult {
    // 鼠标：区域内左键点击 = 选中该项并执行 Enter 动作（click as enter）
    if let Event::Mouse(mouse) = event {
        // 确认删除模式：点击不触发 toggle（防止误删），仅消费事件
        if ctx.confirm_delete.read().is_none()
            && let Some(area) = ctx.area
            && !is_scrollbar_column(&mouse, area)
            && let Some(idx) = hit_item(&mouse, area, ctx.viewport.hit_layout())
        {
            *ctx.selected.write() = idx;
            // 目标取自与命中布局同一份列表快照，避免命中行与待执行目标错位
            if let Some(job) = ctx.jobs.get(idx) {
                action(CRON_TOGGLE_METHOD, &job.id);
            }
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

    // Confirm-delete mode: Enter confirms, Esc cancels, others cancel
    let confirm_target = ctx.confirm_delete.read().clone();
    if let Some(target) = confirm_target {
        match key.code {
            KeyCode::Enter => {
                // 目标按确认时捕获的 id 定位，不再按当前下标重取。
                // 完成门：确认态在本分支内一次性清除——同一次确认不会被重复 Enter 连发删除
                // （第二次 Enter 落到普通模式的 toggle，不会再次 remove）。
                if ctx.jobs.iter().any(|job| job.id == target) {
                    action(CRON_REMOVE_METHOD, &target);
                } else {
                    // 目标已不在列表里（刷新/已被别处删除）：放弃本次删除并退出确认模式。
                    // 删除不可逆，按下标改删「现在这一项」正是要修掉的漂移缺陷。
                    tracing::warn!(
                        target: "cron-panel",
                        job_id = %target,
                        "confirm target no longer in list, delete skipped"
                    );
                    notify_delete_target_missing(&target);
                }
                *ctx.confirm_delete.write() = None;
            }
            KeyCode::Esc => {
                *ctx.confirm_delete.write() = None;
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return EventResult::Ignored;
            }
            _ => {
                *ctx.confirm_delete.write() = None;
            }
        }
        return EventResult::Consumed;
    }

    match key.code {
        KeyCode::Esc => {
            close_panel();
        }
        KeyCode::Up => {
            let mut s = ctx.selected.write();
            *s = previous_selection(*s);
        }
        KeyCode::Down => {
            let mut s = ctx.selected.write();
            if !ctx.jobs.is_empty() {
                *s = next_selection(*s, ctx.jobs.len());
            }
        }
        KeyCode::Enter | KeyCode::Char(' ') => {
            let sel = *ctx.selected.read();
            if let Some(job) = ctx.jobs.get(sel) {
                action(CRON_TOGGLE_METHOD, &job.id);
            }
        }
        KeyCode::Char('d') => {
            // 进入确认模式即固定目标身份（列表为空或选中项越界时不进入）
            let sel = *ctx.selected.read();
            if let Some(job) = ctx.jobs.get(sel) {
                *ctx.confirm_delete.write() = Some(job.id.clone());
            }
        }
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            return EventResult::Ignored;
        }
        _ => {}
    }
    EventResult::Consumed
}

/// 事件处理上下文：本帧视口 + 面板句柄 + 列表快照。
///
/// `State<T>` 无公开构造 API，只能从组件挂载取得；集中成一个结构体，事件入口
/// 签名不再随状态/视口增长（渲染与鼠标命中必须共用同一个 `viewport`）。
struct CronEventCtx<'a> {
    area: Option<Rect>,
    selected: State<usize>,
    /// 待删任务的稳定身份（job.id）；None = 不在确认模式
    confirm_delete: State<Option<String>>,
    viewport: ListViewport,
    jobs: &'a [CronJobSummary],
}

fn close_panel() {
    // I19-A: 弹栈而非清空整个栈，避免同时打开多个不同组面板时关闭一个会全部关闭
    crate::kit::panel_registry::close_active_panel();
}

#[cfg(test)]
mod tests;
