//! ThreadBrowser 回归：列表工具函数的边界，与会话动作（归档 / 删除）回写前的视图身份复验。
//!
//! [Why] 归档与删除的响应都在 `tokio::spawn` 里回写，而单测进程构造不出可控的 ACP client
//! （`ACP_CLIENT_HANDLE` 是进程级 `OnceLock`，由同进程其它用例决定）。故会话动作用例直接
//! 驱动生产 spawn 内调用的同一对落地函数 `apply_archive_outcome` / `apply_delete_outcome`：
//! 身份捕获与复验语义与生产路径同源；RPC 往返与 spawn 调度本身只能由人工 / 端到端验证覆盖。

use super::*;
use serial_test::serial;

#[test]
fn directory_suffix_keeps_workspace_name_and_unicode_width() {
    assert_eq!(path_tail("/long/仓库/feature", 12), "…库/feature");
    assert_eq!(path_tail("/a", 10), "/a");
    assert_eq!(path_tail("/a", 0), "");
}

#[test]
fn truncation_boundaries_are_display_width_safe() {
    assert_eq!(truncate_text("abcdef", 0), "");
    assert_eq!(truncate_text("abcdef", 1), "…");
    assert_eq!(truncate_text("abcdef", 2), "a…");
    assert_eq!(truncate_text("abcdef", 3), "ab…");
    assert_eq!(truncate_text("仓库名称", 1), "…");
    assert_eq!(truncate_text("仓库名称", 2), "…");
    assert_eq!(truncate_text("仓库名称", 3), "仓…");
    assert_eq!(truncate_text("仓库名称", 4), "仓…");
    assert_eq!(truncate_text("仓库名称", 5), "仓库…");
    assert_eq!(truncate_text("仓库名称", 8), "仓库名称");
    assert_eq!(truncate_text("", 1), "");
    assert_eq!(truncate_text("a", 1), "a");
    assert_eq!(truncate_text("a", 2), "a");
    assert_eq!(truncate_text("界", 2), "界");
    assert_eq!(truncate_text("界面", 3), "界…");
    assert_eq!(truncate_text("abcdef", 4), "abc…");
    assert_eq!(truncate_text("abcdef", 5), "abcd…");
    assert_eq!(truncate_text("abcdef", 6), "abcdef");
    assert_eq!(truncate_text("中文", 4), "中文");
    assert_eq!(truncate_text("中文测试", 6), "中文…");
    assert_eq!(truncate_text("中文测试", 7), "中文测…");
    assert_eq!(truncate_text("中文测试", 8), "中文测试");
    assert_eq!(truncate_text("中文测试", 9), "中文测试");
    assert_eq!(truncate_text("x", 0), "");
    assert_eq!(truncate_text("x", 2), "x");
    assert_eq!(truncate_text("xy", 2), "xy");
}

#[test]
fn selection_visibility_clamps_to_resized_viewport() {
    let mut state = ratatui_kit::components::scroll_view::ScrollViewState::with_offset(
        ratatui_kit::ratatui::layout::Position::new(0, 10),
    );
    keep_selection_visible(&mut state, 3, 20, 5);
    assert_eq!(state.offset().y, 3);
    keep_selection_visible(&mut state, 19, 20, 5);
    assert_eq!(state.offset().y, 15);
    keep_selection_visible(&mut state, 0, 0, 5);
    assert_eq!(state.offset().y, 15);
}

// ── 会话动作回写的视图身份（归档 `a` / 删除 `d`+Enter）──────────────────

/// 面板状态相关 atom 的保存/恢复——全局 atom 是进程级的，断言 panic 也不得污染其它用例。
struct PanelStateGuard {
    threads: Vec<ThreadSummary>,
    error: Option<String>,
    active_session: String,
    bridge_reset: u64,
    scope: ThreadBrowserScope,
    archived: bool,
    notification: Option<String>,
}

impl PanelStateGuard {
    /// 基线：会话 A + 项目视图 + 两条条目 + 一条陈旧错误。
    ///
    /// 列表与错误都预置可辨识内容——被旧响应误写时断言才看得见。
    fn install() -> Self {
        let guard = Self {
            threads: THREAD_LIST.state().read().clone(),
            error: list_error(),
            active_session: ACTIVE_SESSION_ID.state().read().clone(),
            bridge_reset: BRIDGE_RESET_COUNTER.get(),
            scope: THREAD_BROWSER_SCOPE.get(),
            archived: THREAD_BROWSER_ARCHIVED.get(),
            notification: NOTIFICATION
                .state()
                .read()
                .as_ref()
                .map(|notice| notice.message.clone()),
        };
        *THREAD_LIST.state().write() = vec![thread("t1"), thread("t2")];
        THREAD_LIST_ERROR.set(Some("baseline error".to_owned()));
        ACTIVE_SESSION_ID.set("session-a".to_owned());
        BRIDGE_RESET_COUNTER.set(0);
        THREAD_BROWSER_SCOPE.set(ThreadBrowserScope::Project);
        THREAD_BROWSER_ARCHIVED.set(false);
        NOTIFICATION.set(None);
        guard
    }
}

impl Drop for PanelStateGuard {
    fn drop(&mut self) {
        *THREAD_LIST.state().write() = std::mem::take(&mut self.threads);
        THREAD_LIST_ERROR.set(self.error.take());
        ACTIVE_SESSION_ID.set(std::mem::take(&mut self.active_session));
        BRIDGE_RESET_COUNTER.set(self.bridge_reset);
        THREAD_BROWSER_SCOPE.set(self.scope);
        THREAD_BROWSER_ARCHIVED.set(self.archived);
        // `Notification` 不可 Clone：按原消息重新发布。
        NOTIFICATION.set(self.notification.take().map(|message| Notification {
            message,
            until: peri_time::monotonic_now() + Duration::from_secs(2),
        }));
    }
}

fn thread(id: &str) -> ThreadSummary {
    ThreadSummary {
        id: id.to_owned(),
        ..ThreadSummary::default()
    }
}

fn thread_ids() -> Vec<String> {
    THREAD_LIST
        .state()
        .read()
        .iter()
        .map(|entry| entry.id.clone())
        .collect()
}

fn list_error() -> Option<String> {
    THREAD_LIST_ERROR.state().read().clone()
}

fn notified_message() -> Option<String> {
    NOTIFICATION
        .state()
        .read()
        .as_ref()
        .map(|notice| notice.message.clone())
}

/// 归档响应回来时活动会话已切换：整条丢弃，不写列表也不写错误。
#[test]
#[serial]
fn archive_response_from_another_session_does_not_touch_list() {
    let _guard = PanelStateGuard::install();
    // 请求捕获于会话 A
    let view = ThreadListViewKey::current();
    // 响应返回前会话已切换：列表换成新会话的条目与新错误
    ACTIVE_SESSION_ID.set("session-b".to_owned());
    *THREAD_LIST.state().write() = vec![thread("other")];
    THREAD_LIST_ERROR.set(Some("new session error".to_owned()));

    apply_archive_outcome(&view, "t1", Ok(()));

    assert_eq!(
        thread_ids(),
        vec!["other"],
        "另一个会话的列表不得被旧响应改写"
    );
    assert_eq!(
        list_error().as_deref(),
        Some("new session error"),
        "旧响应不得清掉新会话的错误"
    );
}

/// scope 切走后，旧归档响应（含失败）不得落到新列表上。
#[test]
#[serial]
fn archive_response_after_scope_switch_does_not_touch_list() {
    let _guard = PanelStateGuard::install();
    let view = ThreadListViewKey::current();
    THREAD_BROWSER_SCOPE.set(ThreadBrowserScope::Workspace);

    apply_archive_outcome(&view, "t1", Err("boom".to_owned()));
    assert_eq!(
        list_error().as_deref(),
        Some("baseline error"),
        "切 scope 后旧失败不得写错误"
    );

    apply_archive_outcome(&view, "t1", Ok(()));
    assert_eq!(
        thread_ids(),
        vec!["t1", "t2"],
        "切 scope 后旧成功不得移除该条"
    );
}

/// 归档视图切走后同样整条丢弃——目标已属于另一个列表。
#[test]
#[serial]
fn archive_response_after_archived_view_switch_does_not_touch_list() {
    let _guard = PanelStateGuard::install();
    let view = ThreadListViewKey::current();
    THREAD_BROWSER_ARCHIVED.set(true);
    *THREAD_LIST.state().write() = vec![thread("archived-only")];

    apply_archive_outcome(&view, "t1", Ok(()));

    assert_eq!(thread_ids(), vec!["archived-only"]);
    assert_eq!(list_error().as_deref(), Some("baseline error"));
}

/// 身份未变：按原语义移除该条并清错误。
#[test]
#[serial]
fn archive_response_removes_entry_when_view_identity_matches() {
    let _guard = PanelStateGuard::install();
    let view = ThreadListViewKey::current();

    apply_archive_outcome(&view, "t1", Ok(()));

    assert_eq!(thread_ids(), vec!["t2"], "归档成功后该条离开当前列表");
    assert_eq!(list_error().as_deref(), None, "成功后清掉旧错误");
}

/// 删除失败必须让用户看见（`NOTIFICATION`），列表保持不动。
#[test]
#[serial]
fn delete_failure_is_visible_to_the_user() {
    let _guard = PanelStateGuard::install();
    let view = ThreadListViewKey::current();

    apply_delete_outcome(&view, "t1", Err("rpc unavailable".to_owned()));

    assert_eq!(
        notified_message(),
        Some(format!(
            "{}: rpc unavailable",
            i18n::tr("thread-browser-delete-error")
        )),
        "失败必须走用户可见提示"
    );
    assert_eq!(thread_ids(), vec!["t1", "t2"], "失败不改列表");
}

/// 删除成功维持现状语义：不改列表、不提示（条目由后续刷新消失）。
#[test]
#[serial]
fn delete_success_leaves_list_to_the_next_refresh() {
    let _guard = PanelStateGuard::install();
    let view = ThreadListViewKey::current();

    apply_delete_outcome(&view, "t1", Ok(()));

    assert_eq!(thread_ids(), vec!["t1", "t2"]);
    assert_eq!(notified_message(), None);
}

/// 视图身份已变：删除失败也不提示——响应描述的是另一个列表。
#[test]
#[serial]
fn delete_response_after_session_switch_is_discarded() {
    let _guard = PanelStateGuard::install();
    let view = ThreadListViewKey::current();
    ACTIVE_SESSION_ID.set("session-b".to_owned());

    apply_delete_outcome(&view, "t1", Err("rpc unavailable".to_owned()));

    assert_eq!(notified_message(), None, "旧列表的失败不得提示给新会话");
}

/// 身份对象覆盖四个维度，且 bridge 代次变化即失效。
#[test]
#[serial]
fn view_key_captures_session_generation_scope_and_archive_view() {
    let _guard = PanelStateGuard::install();
    ACTIVE_SESSION_ID.set("session-z".to_owned());
    BRIDGE_RESET_COUNTER.set(7);
    THREAD_BROWSER_SCOPE.set(ThreadBrowserScope::All);
    THREAD_BROWSER_ARCHIVED.set(true);

    let view = ThreadListViewKey::current();

    assert_eq!(view.session_id, "session-z");
    assert_eq!(view.bridge_reset, 7);
    assert_eq!(view.scope, ThreadBrowserScope::All);
    assert!(view.archived);
    assert!(view.still_current());
    BRIDGE_RESET_COUNTER.set(8);
    assert!(!view.still_current(), "bridge 重建后旧身份立即失效");
}

// ── 相对时间列与过滤器 chip ────────────────────────────────────────────

/// 相对时间按时长分档：刚刚 / 分钟 / 小时 / 天；缺失更新时间用 `common-na`。
#[test]
fn relative_time_buckets_by_elapsed_minutes() {
    crate::i18n::init(Some("en"));
    let now: chrono::DateTime<chrono::Utc> = peri_time::now_wall().into();

    assert_eq!(format_relative_time(now), "just now");
    assert_eq!(
        format_relative_time(now - chrono::Duration::minutes(5)),
        "5 minutes ago"
    );
    assert_eq!(
        format_relative_time(now - chrono::Duration::minutes(1)),
        "1 minute ago",
        "单数不加 s"
    );
    assert_eq!(
        format_relative_time(now - chrono::Duration::hours(2)),
        "2 hours ago"
    );
    assert_eq!(
        format_relative_time(now - chrono::Duration::hours(23)),
        "23 hours ago"
    );
    assert_eq!(
        format_relative_time(now - chrono::Duration::days(14)),
        "14 days ago",
        "超过一天不再细分小时"
    );

    let missing = ThreadSummary {
        id: "t".to_owned(),
        title: None,
        cwd: "/tmp".to_owned(),
        message_count: 0,
        updated_at: None,
    };
    assert_eq!(relative_time_label(&missing), i18n::tr("common-na"));
}

/// 时间列补白按终端显示宽度：中文时间文本用 `{:<width$}` 会少补空格。
#[test]
fn time_column_pads_by_display_width_not_char_count() {
    assert_eq!(pad_display_width("abc", 6), "abc   ");
    assert_eq!(pad_display_width("刚刚", 6), "刚刚  ", "中文 2 字符占 4 列");
    assert_eq!(pad_display_width("12 天前", 6), "12 天前", "已达标不裁剪");
    assert_eq!(pad_display_width("abcdefgh", 6), "abcdefgh", "超宽不截断");

    assert_eq!(time_column_width(&[]), 6, "空列表取下界");
    assert_eq!(time_column_width(&["刚刚".to_owned()]), 6, "低于下界取下界");
    assert_eq!(
        time_column_width(&["刚刚".to_owned(), "12 天前".to_owned()]),
        7,
        "取最宽条目"
    );
    assert_eq!(
        time_column_width(&["x".repeat(40)]),
        12,
        "超宽时间文本夹在上界"
    );
}

/// chip 两侧留白成块，选中态带底色、未选中态无底色。
#[test]
fn chip_span_marks_selection_with_background() {
    let active = Style::new().bg(ratatui_kit::ratatui::style::Color::Red);
    let idle = Style::new();

    let selected = chip_span("项目".to_owned(), true, active, idle);
    assert_eq!(selected.content.as_ref(), " 项目 ");
    assert_eq!(
        selected.style.bg,
        Some(ratatui_kit::ratatui::style::Color::Red)
    );

    let unselected = chip_span("全部".to_owned(), false, active, idle);
    assert_eq!(unselected.content.as_ref(), " 全部 ");
    assert_eq!(unselected.style.bg, None);
}
