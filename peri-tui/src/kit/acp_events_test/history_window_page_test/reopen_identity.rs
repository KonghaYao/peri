//! 复审 10：窗口身份切换与展示/索引的原子协调（自 `history_window_page_test` 拆分）。

use super::*;
use crate::kit::history_window::PersistentEntryKey;

fn open_context(
    request_id: &str,
    identity: &WindowIdentity,
    display_generation: u64,
) -> WindowRequestContext {
    WindowRequestContext {
        request_id: request_id.into(),
        session_id: identity.session_id.clone(),
        identity: identity.clone(),
        window_generation: 0,
        reserved_bytes: 1024,
        placement: WindowPagePlacement::Tail,
        is_open: true,
        display_generation,
        gap: None,
    }
}

fn dispatch_open(
    state: &mut BridgeState,
    request_id: &str,
    identity: WindowIdentity,
    display_generation: u64,
    items: Vec<WindowPageItem>,
) {
    let context = open_context(request_id, &identity, display_generation);
    dispatch_for_bridge(
        state,
        &AcpEventData::HistoryWindowPageLoaded {
            context: Box::new(context),
            response: Box::new(response(identity, items)),
        },
    );
}

#[test]
#[serial]
fn reopen_with_new_turn_keeps_live_answer_once() {
    use crate::kit::tui_render_unit::{TuiRenderUnit, TuiUserBubble};

    let (mut state, _guard) = state_with_window();
    load_page(
        &mut state,
        "r1",
        WindowPagePlacement::Tail,
        vec![text_item("m1", 10, "one")],
    );
    assert_eq!(committed_texts(&state), vec!["one"]);

    // open 在途：新 turn 已经 flush 到 committed，current_turn 保持为空。
    let display_generation = state.generation;
    state
        .committed
        .push_back(TuiRenderUnit::TuiUserBubble(TuiUserBubble::new(
            "answer".to_string(),
        )));
    push_view_models(&mut state);

    let identity = WindowIdentity::new("s1", "gen-2", 8, "snap-2");
    dispatch_open(
        &mut state,
        "r-open",
        identity,
        display_generation,
        vec![text_item("m1", 10, "one"), text_item("m2", 20, "answer")],
    );
    push_view_models(&mut state);

    let texts = committed_texts(&state);
    assert_eq!(
        texts,
        vec!["one", "answer"],
        "旧页原位替换，已 flush 的新回答保留一次"
    );
    assert_eq!(
        texts
            .iter()
            .filter(|text| text.as_str() == "answer")
            .count(),
        1,
        "新回答不得被 open 页重复插入"
    );
    let adopted = PersistentEntryKey::own("s1:m2");
    assert!(
        state.window.is_entry_displayed(&adopted),
        "已由 live 尾部展示的条目只登记展示身份"
    );
    assert!(
        !state.window.index().contains_entry(&adopted),
        "adopt 不进入驻留索引，不持有 committed 槽位"
    );
    assert_eq!(state.window.displayed_entry_count(), 2);
    assert_eq!(state.window.segment_count(), 1);

    // 后续翻页重新返回同一持久条目时，adopt 身份必须阻止重复插入。
    load_page(
        &mut state,
        "r2",
        WindowPagePlacement::BeforeFirst,
        vec![text_item("m2", 20, "answer")],
    );
    assert_eq!(
        committed_texts(&state)
            .iter()
            .filter(|text| text.as_str() == "answer")
            .count(),
        1,
        "翻页不得重新插入已 adopt 的条目"
    );
}

#[test]
#[serial]
fn adopt_only_ledger_survives_next_identity_switch() {
    use crate::kit::tui_render_unit::{TuiRenderUnit, TuiUserBubble};

    let (mut state, _guard) = state_with_window();
    load_page(
        &mut state,
        "r1",
        WindowPagePlacement::Tail,
        vec![text_item("m1", 10, "one")],
    );

    // 新 turn 已 flush：live 尾部进入 committed。
    let display_generation = state.generation;
    state
        .committed
        .push_back(TuiRenderUnit::TuiUserBubble(TuiUserBubble::new(
            "answer".to_string(),
        )));
    push_view_models(&mut state);

    // 第二个身份只返回已由 live 尾部展示的条目：adopt-only，不建段。
    let identity = WindowIdentity::new("s1", "gen-2", 8, "snap-2");
    // 模拟 `settle_window_response` 已把 open 响应边界写入 consumer。
    crate::kit::atoms::HISTORY_WINDOW_PAGING
        .state()
        .write()
        .before_cursor = Some("before-2".into());
    dispatch_open(
        &mut state,
        "r-open-2",
        identity,
        display_generation,
        vec![text_item("m2", 20, "answer")],
    );
    push_view_models(&mut state);
    assert_eq!(state.window.displayed_entry_count(), 1);
    assert_eq!(state.window.segment_count(), 0);
    assert_eq!(
        crate::kit::atoms::HISTORY_WINDOW_PAGING
            .state()
            .read()
            .before_cursor
            .as_deref(),
        Some("before-2"),
        "adopt-only open 不得用空布局边缘覆盖响应写入的游标"
    );

    // 再来一次 flush 后，第三个身份返回上一轮回答与新回答。
    state
        .committed
        .push_back(TuiRenderUnit::TuiUserBubble(TuiUserBubble::new(
            "third".to_string(),
        )));
    push_view_models(&mut state);
    let display_generation = state.generation;
    let identity = WindowIdentity::new("s1", "gen-3", 9, "snap-3");
    dispatch_open(
        &mut state,
        "r-open-3",
        identity,
        display_generation,
        vec![text_item("m2", 20, "answer"), text_item("m3", 30, "third")],
    );
    push_view_models(&mut state);

    assert_eq!(committed_texts(&state), vec!["answer", "third"]);
    assert_eq!(
        state.window.displayed_entry_count(),
        2,
        "adopt 身份在再次身份切换后仍须保留"
    );
    assert_eq!(state.window.segment_count(), 0, "adopt-only 链不物化正文段");
}

#[test]
#[serial]
fn adopt_only_reopen_places_older_history_before_live_tail() {
    use crate::kit::tui_render_unit::{TuiRenderUnit, TuiUserBubble};

    let (mut state, _guard) = state_with_window();
    load_page(
        &mut state,
        "r1",
        WindowPagePlacement::Tail,
        vec![text_item("m1", 10, "one")],
    );

    let display_generation = state.generation;
    state
        .committed
        .push_back(TuiRenderUnit::TuiUserBubble(TuiUserBubble::new(
            "answer".to_string(),
        )));
    push_view_models(&mut state);

    let identity = WindowIdentity::new("s1", "gen-2", 8, "snap-2");
    dispatch_open(
        &mut state,
        "r-open-2",
        identity,
        display_generation,
        vec![text_item("m2", 20, "answer")],
    );
    push_view_models(&mut state);

    state
        .committed
        .push_back(TuiRenderUnit::TuiUserBubble(TuiUserBubble::new(
            "third".to_string(),
        )));
    push_view_models(&mut state);
    let display_generation = state.generation;
    let identity = WindowIdentity::new("s1", "gen-3", 9, "snap-3");
    dispatch_open(
        &mut state,
        "r-open-3",
        identity,
        display_generation,
        vec![
            text_item("m1", 10, "one"),
            text_item("m2", 20, "answer"),
            text_item("m3", 30, "third"),
        ],
    );
    push_view_models(&mut state);

    assert_eq!(
        committed_texts(&state),
        vec!["one", "answer", "third"],
        "adopt-only 之后重取的历史必须插到 live 尾部之前"
    );
}

/// 流式 turn 期间重开：账本只有 adopt 身份时，open 页的更早历史仍须插到
/// live 尾部之前，不能因为 `live_empty == false` 而追加到末尾。
#[test]
#[serial]
fn streaming_reopen_places_older_history_before_live_tail() {
    use crate::kit::tui_render_unit::{TuiRenderUnit, TuiUserBubble};

    let (mut state, _guard) = state_with_window();
    load_page(
        &mut state,
        "r1",
        WindowPagePlacement::Tail,
        vec![text_item("m1", 10, "one")],
    );

    let display_generation = state.generation;
    state
        .committed
        .push_back(TuiRenderUnit::TuiUserBubble(TuiUserBubble::new(
            "answer".to_string(),
        )));
    push_view_models(&mut state);

    // adopt-only：window 只登记身份，不建段；committed 保留 live 尾部。
    let identity = WindowIdentity::new("s1", "gen-2", 8, "snap-2");
    dispatch_open(
        &mut state,
        "r-open-2",
        identity,
        display_generation,
        vec![text_item("m2", 20, "answer")],
    );
    push_view_models(&mut state);
    assert_eq!(state.window.displayed_entry_count(), 1);
    assert_eq!(state.window.segment_count(), 0);

    // 新 turn 正在流式输出：live 区域非空，adopt 不启用。
    state.current_turn.append_text("streaming", Some("m-live"));
    let display_generation = state.generation;
    let identity = WindowIdentity::new("s1", "gen-3", 9, "snap-3");
    dispatch_open(
        &mut state,
        "r-open-3",
        identity,
        display_generation,
        vec![text_item("m0", 5, "zero")],
    );
    push_view_models(&mut state);

    assert_eq!(
        committed_texts(&state),
        vec!["zero", "answer"],
        "更早的历史必须插到 live 尾部之前"
    );
}

#[test]
#[serial]
fn reopen_after_streaming_turn_does_not_duplicate_prior_page() {
    use crate::kit::tui_render_unit::{TuiRenderUnit, TuiUserBubble};

    let (mut state, _guard) = state_with_window();
    load_page(
        &mut state,
        "r1",
        WindowPagePlacement::Tail,
        vec![text_item("m1", 10, "one")],
    );

    let display_generation = state.generation;
    state
        .committed
        .push_back(TuiRenderUnit::TuiUserBubble(TuiUserBubble::new(
            "new question".to_string(),
        )));
    push_view_models(&mut state);
    state.current_turn.append_text("streaming", Some("m-live"));

    let identity = WindowIdentity::new("s1", "gen-2", 8, "snap-2");
    dispatch_open(
        &mut state,
        "r-open",
        identity,
        display_generation,
        vec![text_item("m2", 20, "two")],
    );
    push_view_models(&mut state);

    assert_eq!(
        committed_texts(&state),
        vec!["two", "new question"],
        "旧窗口页被精确摘除，live 尾部保留，open 页插到 live 之前"
    );
    assert!(
        state.current_turn.text.contains("streaming"),
        "窗口切换不得改写正在流式的 live 区域"
    );

    load_page(
        &mut state,
        "r-before",
        WindowPagePlacement::BeforeFirst,
        vec![text_item("m1", 10, "one")],
    );
    assert_eq!(
        committed_texts(&state)
            .iter()
            .filter(|text| text.as_str() == "one")
            .count(),
        1,
        "旧 committed 不得因身份切换失去去重保护"
    );
}

#[test]
#[serial]
fn same_identity_open_retry_preserves_display_and_dedupes() {
    let (mut state, _guard) = state_with_window();
    load_page(
        &mut state,
        "r1",
        WindowPagePlacement::Tail,
        vec![text_item("m1", 10, "one")],
    );
    state.window.set_degraded("cursor expired");
    let identity = state.window.identity().cloned().expect("identity");
    let display_generation = state.generation;

    dispatch_open(
        &mut state,
        "r-open-same",
        identity,
        display_generation,
        vec![text_item("m1", 10, "one"), text_item("m2", 20, "two")],
    );
    push_view_models(&mut state);

    assert_eq!(committed_texts(&state), vec!["one", "two"]);
    assert_eq!(state.window.displayed_entry_count(), 2);
    assert_eq!(
        state.window.segment_count(),
        2,
        "旧段保留，重试只合并新页（m1 被展示身份去重）"
    );
    assert!(!state.window.is_degraded(), "成功的 open 必须恢复 Ready");
    assert!(
        !crate::kit::atoms::HISTORY_WINDOW_PAGING
            .state()
            .read()
            .is_degraded(),
        "consumer 也必须在 open 成功后离开 Degraded"
    );
}

#[test]
#[serial]
fn reopen_with_inconsistent_ledger_degrades_without_mutation() {
    let (mut state, _guard) = state_with_window();
    let identity_before = state.window.identity().cloned().expect("identity");
    let generation_before = state.window.generation();
    // 账本区间远超 committed：不得猜测性删除。
    state
        .window
        .mark_display_block(&[(PersistentEntryKey::own("s1:broken"), 2)], 100);

    let identity = WindowIdentity::new("s1", "gen-2", 8, "snap-2");
    let display_generation = state.generation;
    dispatch_open(
        &mut state,
        "r-open-broken",
        identity,
        display_generation,
        vec![text_item("m1", 10, "one")],
    );

    assert_eq!(
        state.window.identity(),
        Some(&identity_before),
        "失败不得切换窗口身份"
    );
    assert_eq!(
        state.window.generation(),
        generation_before,
        "失败不得推进窗口代次"
    );
    assert!(state.committed.is_empty(), "失败不得修改 committed");
    assert_eq!(
        state.window.displayed_entry_count(),
        1,
        "原账本保持原样，等待会话边界 reset"
    );
    assert!(state.window.is_degraded(), "失败必须显式进入 Degraded");
    let consumer = crate::kit::atoms::HISTORY_WINDOW_PAGING
        .state()
        .read()
        .clone();
    assert!(consumer.is_degraded(), "consumer 必须等待 open 重试");
    assert!(consumer.pending_merge.is_none());
}
