//! `viewport_anchor` 的验收级用例：**真实发布路径**上的 A20「阅读锚点不跳动」。
//!
//! 与同目录单元测试的区别：这里的窗口不是手工构造的 `SlotIndex`，而是
//!
//! 1. 桥接事件（`ReplayEntryMarked` / `ReplayedUserBubble` / `HistoryPageLoaded`）
//!    经 `handle_history_page_loaded` 真的并入 `state.committed`；
//! 2. `push_view_models` 真的发布 `VIEW_MODELS.items`（含 `group_successful_tools`
//!    折叠 pass——相邻成功工具卡被压成一张 `TuiCollapsedGroup`，**桥接槽位数与渲染
//!    槽位数因此不相等**，这是 S7d 判 FAIL 的反例形状）；
//! 3. `Transcript::prepare` 用该发布快照做发布前捕获 / 发布后重定位（渲染侧生产路径）。
//!
//! 判据仍只有一条：同一个可见单元在屏幕上的行号不变（`slot_visual_start − offset`）。

use super::*;
use crate::kit::acp_events::{
    BridgeState, acp_events_test::make_fold_test_state, dispatch_and_notify,
};
use crate::kit::acp_types::AcpEventData;
use crate::kit::entry_render_cache::{EntryRenderContext, EntrySurface};
use crate::kit::history_paging::{self, HistoryPageItem, HistoryPageResponse, HistorySubItem};
use crate::kit::message_area::grid::GridSpec;
use crate::kit::message_area::selection::SlotIndex;
use crate::kit::message_area::transcript::Transcript;
use crate::kit::message_area::vm_cache::VmCacheSlot;
use crate::kit::tui_render_unit::{TuiRenderUnit, TuiUserBubble};
use peri_theme::atoms::THEME_ATOM;
use serde_json::json;
use std::sync::Arc;

fn context() -> EntryRenderContext {
    crate::kit::atoms::init_atoms();
    crate::i18n::init(Some("en"));
    EntryRenderContext {
        theme: Arc::clone(&THEME_ATOM.state().read()),
        language: crate::kit::atoms::LANG_VERSION.get(),
        surface: EntrySurface::Message,
        occurrence: 0,
    }
}

fn sub(position: u64, update: serde_json::Value) -> HistorySubItem {
    HistorySubItem {
        position,
        update,
        system_reminder: false,
    }
}

fn page(
    token: &str,
    cursor: Option<&str>,
    items: Vec<(&str, u64, Vec<HistorySubItem>)>,
) -> HistoryPageResponse {
    HistoryPageResponse {
        session_id: "s1".to_string(),
        snapshot_token: token.to_string(),
        next_cursor: cursor.map(str::to_owned),
        items: items
            .into_iter()
            .map(|(entry_id, position, sub_items)| HistoryPageItem {
                entry_id: entry_id.to_string(),
                position,
                oversize: false,
                sub_items,
            })
            .collect(),
    }
}

fn user_update(text: &str) -> serde_json::Value {
    json!({
        "sessionUpdate": "user_message_chunk",
        "content": { "type": "text", "text": text },
        "messageId": "m",
    })
}

fn tool_start(tool_id: &str, tool_name: &str) -> serde_json::Value {
    json!({
        "sessionUpdate": "tool_call",
        "toolCallId": tool_id,
        "title": tool_name,
        "rawInput": { "file_path": "/tmp/x" },
    })
}

fn tool_end(tool_id: &str, output: &str, is_error: bool) -> serde_json::Value {
    json!({
        "sessionUpdate": "tool_call_update",
        "toolCallId": tool_id,
        "rawOutput": output,
        "status": if is_error { "failed" } else { "completed" },
    })
}

/// 浏览态桥接 fixture：会话 `s1` + 快照 `tok`（与 `history_page_test.rs` 同口径）。
fn browsing_state(token: &str) -> BridgeState {
    crate::kit::atoms::init_atoms();
    history_paging::reset_browsing_state();
    *crate::kit::atoms::ACTIVE_SESSION_ID.state().write() = "s1".to_string();
    let mut state = make_fold_test_state();
    state.active_session_id = "s1".to_string();
    history_paging::note_initial_cursor(
        "s1",
        Some(token.to_string()),
        Some("cursor-1".to_string()),
    );
    state
}

/// 首屏自有条目：回放标记（登记 position）+ 一条 user 气泡（进 committed）。
fn seed_own_entry(state: &mut BridgeState, entry_id: &str, position: u64, text: &str) {
    seed_own_entry_via(state, entry_id, position, |state| {
        dispatch_and_notify(
            state,
            &AcpEventData::ReplayedUserBubble {
                input_id: format!("input-{entry_id}"),
                text: text.to_string(),
            },
        );
    });
}

/// 首屏自有条目：回放标记（登记 position）+ `push_unit` 推入该条目的展示单元。
///
/// `ReplayEntryMarked` 与「推入恰好一个单元」的配对由 `acp_events/mod.rs` 的
/// span 登记负责（工具卡也在集合里）。
fn seed_own_entry_via(
    state: &mut BridgeState,
    entry_id: &str,
    position: u64,
    push_unit: impl FnOnce(&mut BridgeState),
) {
    dispatch_and_notify(
        state,
        &AcpEventData::ReplayEntryMarked {
            entry_id: entry_id.to_string(),
            position,
        },
    );
    push_unit(state);
}

/// 继承前缀：无 `position` 的祖先前缀（没有回放标记，因此不进区间索引），
/// 首屏 load 时就在窗口里。更早的页必须插在它**之后**（REQ-H01-6）。
fn seed_inherited_prefix(state: &mut BridgeState, text: &str) {
    state.committed = im::Vector::from(vec![TuiRenderUnit::TuiUserBubble(TuiUserBubble::new(
        text.to_string(),
    ))]);
}

/// 派发一页并**要求落地**：浏览身份（进程级 atom）会被并行用例改写，先钉回来。
fn dispatch_landing_page(state: &mut BridgeState, token: &str, response: HistoryPageResponse) {
    {
        let handle = crate::kit::atoms::HISTORY_PAGING.state();
        let mut paging = handle.write();
        paging.session_id = "s1".to_string();
        paging.snapshot_token = Some(token.to_string());
    }
    assert!(
        crate::kit::atoms::HISTORY_PAGING
            .state()
            .read()
            .accepts("s1", token),
        "浏览身份在派发前被并行用例改写（测试间共享 atom 干扰）"
    );
    dispatch_and_notify(
        state,
        &AcpEventData::HistoryPageLoaded {
            response: Box::new(response),
        },
    );
}

/// 渲染侧一次「发布前捕获 → 发布后重定位」的观测结果。
struct RelocationProbe {
    before: Arc<SlotIndex>,
    after: Arc<SlotIndex>,
    scroll_y: usize,
    relocation: PrependRelocation,
}

impl RelocationProbe {
    /// 目标偏移（浏览态、相对量口径）——effect 会把它落到 `scroll_state` 上。
    fn target(&self) -> usize {
        self.relocation
            .target_for(false, self.scroll_y)
            .expect("浏览态必须换算")
    }

    /// 屏幕行 = 单元视觉起始 − 视口偏移。
    fn screen_row(&self, index: &SlotIndex, slot: usize, offset: usize) -> isize {
        index.slot_visual_start(slot).expect("单元视觉起始") as isize - offset as isize
    }
}

/// 跑一轮真实发布：首屏发布 → `prepare` → 并入一页发布 → `prepare`（锚点落在
/// `anchor_slot` 顶行之下 1 行的位置）。
///
/// `prepare` 的参数与 `message_area/mod.rs` 的生产调用一致（同一份 grid / context /
/// reset，`scroll_y` 取当帧视口事实，`merge_epoch` 取 `HISTORY_PREPEND_EPOCH`）。
fn publish_then_relocate(
    state: &mut BridgeState,
    token: &str,
    anchor_slot: usize,
    response: HistoryPageResponse,
) -> RelocationProbe {
    let grid = GridSpec::grid_for(80);
    let context = context();
    let reset = crate::kit::atoms::BRIDGE_RESET_COUNTER.get();
    let mut transcript = Transcript::default();
    let mut caches: Vec<VmCacheSlot> = Vec::new();

    // 首屏发布后的第一帧：建立旧布局（锚点就在这份索引上捕获）。
    let first = crate::kit::atoms::VIEW_MODELS.state().read().clone();
    let before = transcript.prepare(
        &first,
        &crate::kit::atoms::TRANSCRIPT_PUBLICATION.get(),
        &mut caches,
        grid,
        grid.line_width(),
        context.clone(),
        0,
        reset,
        0,
        10,
        crate::kit::atoms::HISTORY_PREPEND_EPOCH.get(),
    );
    let epoch_before = crate::kit::atoms::HISTORY_PREPEND_EPOCH.get();
    // 视口顶行落在锚点单元内（+1 行），这样「单元内位移」参与判据，不能退化成 0。
    let scroll_y = before.slot_visual_start(anchor_slot).expect("锚点单元起始") + 1;

    dispatch_landing_page(state, token, response);
    assert!(
        crate::kit::atoms::HISTORY_PREPEND_EPOCH.get() > epoch_before,
        "并入成功必须推进并合代数（跨层唯一事实）"
    );

    // 并入后的发布帧：`prepare` 内部完成捕获 + 重定位。
    let second = crate::kit::atoms::VIEW_MODELS.state().read().clone();
    assert_ne!(
        second.generation, first.generation,
        "并入必须产生新一次发布"
    );
    let after = transcript.prepare(
        &second,
        &crate::kit::atoms::TRANSCRIPT_PUBLICATION.get(),
        &mut caches,
        grid,
        grid.line_width(),
        context,
        0,
        reset,
        scroll_y,
        10,
        crate::kit::atoms::HISTORY_PREPEND_EPOCH.get(),
    );
    let relocation = transcript
        .take_prepend_relocation()
        .expect("并入帧必须产出重定位（身份认得出来）");
    RelocationProbe {
        before,
        after,
        scroll_y,
        relocation,
    }
}

/// 场景 A（插入点在**视口上方**，翻页常见形态）：视口顶行所在的单元被整块下推，
/// 重定位把它按同一屏幕行钉回原位。
#[test]
#[serial_test::serial]
fn a20_e2e_insert_above_viewport_keeps_the_anchored_unit_screen_row() {
    let mut state = browsing_state("tok");
    seed_inherited_prefix(&mut state, "inherited");
    seed_own_entry(&mut state, "s1:own", 100, "first-screen-own");
    let short = page(
        "tok",
        None,
        vec![("s1:old", 10, vec![sub(0, user_update("old one"))])],
    );

    let probe = publish_then_relocate(&mut state, "tok", 1, short);
    let before_slot = 1;
    // 渲染窗口 = 继承前缀 + 更早条目 + 自有条目 → 自有条目落到槽位 2。
    let after_slot = 2;
    assert_eq!(probe.after.slot_count(), 3, "插入一个单元的发布");
    assert_eq!(
        probe.screen_row(&probe.after, after_slot, probe.target()),
        probe.screen_row(&probe.before, before_slot, probe.scroll_y),
        "场景 A：同一可见单元屏幕行不变（A20）"
    );
    assert!(
        probe.target() > probe.scroll_y,
        "插入块在上方 → 目标偏移必须增大（真的补偿了）"
    );
}

/// 场景 B（**中间缺口填补**，插入点在视口下方）：上方内容没有任何行被推动，
/// 重定位量必须是 0——旧「按插入高度一律补偿」口径会在这里把画面推走
/// （S7c 复核 §8.2 实测：−4 行）。
#[test]
#[serial_test::serial]
fn a20_e2e_middle_gap_fill_does_not_move_the_viewport() {
    let mut state = browsing_state("tok");
    // 首屏：a(position 10) 与 b(position 100)。视口顶行停在 a。
    seed_own_entry(&mut state, "s1:a", 10, "own-a");
    seed_own_entry(&mut state, "s1:b", 100, "own-b");
    let gap = page(
        "tok",
        Some("cursor-2"),
        vec![("s1:mid", 50, vec![sub(0, user_update("between"))])],
    );

    let probe = publish_then_relocate(&mut state, "tok", 0, gap);
    assert_eq!(probe.after.slot_count(), 3, "缺口页插到 a 与 b 之间");
    assert_eq!(
        probe.target(),
        probe.scroll_y,
        "插入点在视口下方 → 重定位量为 0（视口不动）"
    );
    assert_eq!(
        probe.screen_row(&probe.after, 0, probe.target()),
        probe.screen_row(&probe.before, 0, probe.scroll_y),
        "场景 B：同一可见单元屏幕行不变（A20）"
    );
}

/// S7d 反例形状（**跨层槽位坐标**不复存在）：相邻成功工具卡被折叠成一张
/// `TuiCollapsedGroup`，桥接 `committed` 槽位数 ≠ 渲染 `VIEW_MODELS.items` 槽位数。
/// 旧路径用桥接槽位包络去索引渲染槽位，在**这次发布**上会过补偿；身份重定位不看
/// 槽位号，锚点（自有气泡）的屏幕行必须不变。
#[test]
#[serial_test::serial]
fn a20_e2e_collapsed_group_shape_keeps_the_anchored_unit_screen_row() {
    let mut state = browsing_state("tok");
    seed_inherited_prefix(&mut state, "inherited");
    seed_own_entry(&mut state, "s1:own", 100, "first-screen-own");
    // 两张成功工具卡，起止分处两条目（生产形状：一个持久 payload 只发射起点或终点）。
    let tools = page(
        "tok",
        Some("cursor-2"),
        vec![
            ("s1:old-1a", 10, vec![sub(0, tool_start("t1", "Read"))]),
            ("s1:old-1b", 11, vec![sub(0, tool_end("t1", "ok-1", false))]),
            ("s1:old-2a", 20, vec![sub(0, tool_start("t2", "Grep"))]),
            ("s1:old-2b", 21, vec![sub(0, tool_end("t2", "ok-2", false))]),
        ],
    );

    let probe = publish_then_relocate(&mut state, "tok", 1, tools);

    // 两把尺子不同的实测事实（同一份发布，同一时刻）：
    //   桥接 committed = [继承, 卡 t1, 卡 t2, 自有] = 4 槽；
    //   渲染 VIEW_MODELS = [继承, GROUP(2 张卡), 自有] = 3 槽。
    assert_eq!(
        state.committed.len(),
        4,
        "桥接槽位数（插入块在桥接坐标里占 [1, 3)）"
    );
    assert_eq!(
        probe.after.slot_count(),
        3,
        "渲染槽位数少 1——相邻成功工具卡被压成折叠组"
    );
    assert!(
        matches!(
            crate::kit::atoms::VIEW_MODELS.state().read().items.get(1),
            Some(TuiRenderUnit::TuiCollapsedGroup(_))
        ),
        "槽位 1 必须是折叠组"
    );

    // 锚点身份（自有气泡）在渲染坐标里的下标是 2，在桥接坐标里是 3——**不同**，
    // 这正是旧路径错位的原因；新路径只认身份。
    let own_in_render = 2usize;
    let inserted_rows = probe
        .after
        .slot_visual_start(own_in_render)
        .expect("自有单元起始")
        - probe.after.slot_visual_start(1).expect("折叠组起始");
    assert_eq!(inserted_rows, 1, "折叠组在渲染布局里只占 1 行");
    assert_eq!(
        probe.screen_row(&probe.after, own_in_render, probe.target()),
        probe.screen_row(&probe.before, 1, probe.scroll_y),
        "折叠组形态：同一可见单元屏幕行不变（A20）"
    );
    assert_eq!(
        probe.target() as isize - probe.scroll_y as isize,
        inserted_rows as isize,
        "重定位量 = 插入块在**渲染布局**里的真实高度（1 行），而不是桥接槽位数（2 张卡 = 2 槽）"
    );
}

/// 第二种跨层重排形态：**插入把已有的折叠组拆开**。首屏两张相邻成功工具卡被压成
/// 一张组；更早一页的条目插到两张卡中间 → 组消失、槽位变多，锚点（下方自有气泡）
/// 被整体下推。锚点身份仍是同一个单元 → 重定位把它钉回原屏幕行。
#[test]
#[serial_test::serial]
fn a20_e2e_group_split_keeps_the_anchored_unit_screen_row() {
    let mut state = browsing_state("tok");
    // 首屏两条自有工具条目（各一条成功卡）+ 一条自有气泡；两张卡相邻 → 渲染侧成组。
    seed_own_entry_via(&mut state, "s1:c1", 10, |state| {
        dispatch_and_notify(
            state,
            &AcpEventData::ReplayToolStarted {
                tool_id: "t1".to_string(),
                tool_name: "Read".to_string(),
                input_summary: "a.rs".to_string(),
                raw_input: json!({"file_path": "a.rs"}),
            },
        );
        dispatch_and_notify(
            state,
            &AcpEventData::ReplayToolEnded {
                tool_id: "t1".to_string(),
                output_summary: "ok-1".to_string(),
                is_error: false,
            },
        );
    });
    seed_own_entry_via(&mut state, "s1:c2", 20, |state| {
        dispatch_and_notify(
            state,
            &AcpEventData::ReplayToolStarted {
                tool_id: "t2".to_string(),
                tool_name: "Grep".to_string(),
                input_summary: "b.rs".to_string(),
                raw_input: json!({"pattern": "b"}),
            },
        );
        dispatch_and_notify(
            state,
            &AcpEventData::ReplayToolEnded {
                tool_id: "t2".to_string(),
                output_summary: "ok-2".to_string(),
                is_error: false,
            },
        );
    });
    seed_own_entry(&mut state, "s1:own", 100, "first-screen-own");

    // 一条更早条目落在两张卡之间（position 15 ∈ (10, 20)）。
    let split = page(
        "tok",
        Some("cursor-2"),
        vec![("s1:mid", 15, vec![sub(0, user_update("between-cards"))])],
    );

    let probe = publish_then_relocate(&mut state, "tok", 1, split);

    // 发布前：两张卡相邻 → 一张折叠组，渲染侧槽位 = [GROUP(2 张卡), 自有]。
    assert!(
        matches!(
            probe
                .before
                .canonical_items()
                .and_then(|items| items.get(0)),
            Some(TuiRenderUnit::TuiCollapsedGroup(_))
        ),
        "首屏两张成功卡必须已经成组"
    );
    // 发布后：组被拆开 → 槽位变多（组 → 卡 + 中间条目 + 卡）。
    let own_after = 3usize;
    assert!(
        probe.after.slot_count() > probe.before.slot_count(),
        "拆组后槽位数必须变多（{} → {}）",
        probe.before.slot_count(),
        probe.after.slot_count()
    );
    assert_eq!(
        probe.screen_row(&probe.after, own_after, probe.target()),
        probe.screen_row(&probe.before, 1, probe.scroll_y),
        "拆组形态：同一可见单元屏幕行不变（A20）"
    );
    assert!(
        probe.target() > probe.scroll_y,
        "拆组后锚点上方内容变高 → 目标偏移必须增大"
    );
}
