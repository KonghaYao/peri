//! Render helper functions — push_view_models, push_acp_state, etc.

use super::super::{BridgeState, SessionPhase};

use crate::i18n;
use crate::kit::atoms::{VIEW_MODELS, ViewModelsSnapshot};

use crate::kit::tui_render_unit::FoldKey;
use crate::kit::tui_render_unit::FoldState;
use crate::kit::tui_render_unit::TuiCollapsedGroup;
use crate::kit::tui_render_unit::TuiDivider;
use crate::kit::tui_render_unit::TuiNoteLevel;
use crate::kit::tui_render_unit::TuiRenderUnit;
use crate::kit::tui_render_unit::TuiTodoSummary;
use crate::kit::tui_render_unit::same_render_unit;

#[path = "history_group.rs"]
mod history_group;
use fluent_bundle::FluentValue;
use history_group::{group_prepended_page, rebuild_grouped_suffix};
// These replaceable derived snapshots are committed through whole-value assignments.
use parking_lot::Mutex;

/// 发布构建的重试上限：构建期间若点击路径改写了当帧快照（generation 前进），
/// 本轮构建结果作废并重建；上限内仍冲突则退化为持写锁构建。
const PUBLICATION_BUILD_RETRIES: usize = 3;

/// 将 BridgeState 中的 ViewModels 写入 VIEW_MODELS Atom。
///
/// 从 `state.committed`（im::Vector）clone（O(1)引用计数）后并入
/// `current_turn.view_models()`（见 [`join_into`]：按尾长选择 `push_back` 或树合并，
/// 与 current_turn 增量缓存共享元素，不逐条深拷贝），构成扁平单层列表。
/// generation 每次发布递增+1。
///
/// 快照后处理流水线（均为纯视觉变换，不触碰 segment↔cache 对齐）：
/// 1. `apply_fold_pass`（spec §7 表 + FOLD_OVERRIDES）；
/// 2. 按完整消息顺序投影 turn 分隔线（随下一条用户 prompt 保留）；
/// 3. todo 进度摘要行（§6.9：TODO_ITEMS 派生，插在最终回答前）；
/// 4. `group_body_gaps`（正文空档分组：正文段落之间的工具 / thinking / 通知等
///    合成一张折叠组，一组一行）。
///
/// [并发] 点击路径（`apply_fold_toggle`）持 `VIEW_MODELS` 写锁写 `FOLD_OVERRIDES`
/// 并推进当帧快照的 generation；发布是「读覆盖表 → 组装 → 整体写入」，若点击落在
/// 组装之后、写锁之前，旧折叠结果会覆盖点击结果且回合末不再发布——展开态永久
/// 丢失。因此提交是**有界重试**：起点 generation 在读取覆盖表之前取一次，写锁内
/// 比对 generation，冲突就丢弃本轮构建重建（上限 [`PUBLICATION_BUILD_RETRIES`]）。
/// 锁序恒为 `VIEW_MODELS → FOLD_OVERRIDES`，与点击一致（见 [`build_publication_snapshot`]）。
pub(crate) fn push_view_models(state: &mut BridgeState) {
    let reset = crate::kit::atoms::BRIDGE_RESET_COUNTER.get();
    // ── 一次性 housekeeping：每次发布只执行一次，快照构建重试不得重复执行 ──
    super::super::subagent_preview::enforce(state);
    super::super::display_retention::enforce(state);
    state
        .window
        .ack_release_epoch(crate::kit::atoms::RENDER_ACHIEVED_EPOCH.get());
    crate::kit::atoms::HISTORY_WINDOW_STATS.set(state.window.diagnostics());
    // running child 可以阻止归档，但主 turn 的终态时长仍只冻结一次。
    if state.phase != SessionPhase::PromptRunning {
        state.current_turn.freeze_trailing();
    }
    // [Diagnostic] 追踪 VIEW_MODELS 写入时机——配合 scroll diag 分析 submit/history 滚动问题。
    // trace 级别：每 token 调用一次，默认 info filter 下不落盘。
    let is_loading = state.phase == SessionPhase::PromptRunning;
    tracing::trace!(
        target: "msg_scroll_diag",
        committed = state.committed.len(),
        current_turn = state.current_turn.view_model_count(),
        generation = state.generation,
        phase = ?state.phase,
        is_loading,
        "push_view_models: writing VIEW_MODELS atom",
    );

    let view_models = VIEW_MODELS.state();
    // [并发/锁序] 起点 generation 必须在读 FOLD_OVERRIDES 之前捕获，且只取一次
    // VIEW_MODELS 读锁后立即释放：点击是 `VIEW_MODELS 写 → FOLD_OVERRIDES 写`，
    // 构建段内若再嵌套 `FOLD_OVERRIDES → VIEW_MODELS` 就构成 ABBA。
    let mut build_start_generation = view_models.read().generation;
    let mut retries = 0usize;
    let committed = loop {
        let (items, changed_from) = build_publication_snapshot(state);
        let page =
            crate::kit::message_area::history_layout::measure_page(build_start_generation, &items);
        // [测试] 构建完成、尚未取写锁——交错点可确定性地模拟并发点击。
        #[cfg(test)]
        run_publication_interleave_hook(state);
        let mut published = view_models.write();
        if crate::kit::atoms::BRIDGE_RESET_COUNTER.get() != reset {
            break false;
        }
        if published.generation == build_start_generation {
            commit_publication(
                state,
                &mut published,
                items,
                changed_from,
                build_start_generation,
            );
            crate::kit::message_area::history_layout::publish(published.generation, page);
            break true;
        }
        drop(published);
        if retries >= PUBLICATION_BUILD_RETRIES {
            // 兜底：持写锁构建提交。写锁内只读 FOLD_OVERRIDES（`VM → FO`，与点击
            // 同序）；覆盖表旁的缓存锁（分组 / 分隔线 / todo 摘要）不存在「持缓存锁
            // 再取 VIEW_MODELS」的路径，不引入新的反序。
            let mut published = view_models.write();
            let fallback_start_generation = published.generation;
            let (items, changed_from) = build_publication_snapshot(state);
            if crate::kit::atoms::BRIDGE_RESET_COUNTER.get() != reset {
                break false;
            }
            commit_publication(
                state,
                &mut published,
                items,
                changed_from,
                fallback_start_generation,
            );
            crate::kit::message_area::history_layout::publish(published.generation, None);
            break true;
        }
        retries += 1;
        build_start_generation = view_models.read().generation;
    };
    if !committed {
        state.folded_history.invalidate();
        invalidate_structural_caches();
        return;
    }
    tracing::trace!(target: "frozen_diag", "bridge: wrote VIEW_MODELS");
    super::super::history_window::settle_after_publication(state);
}

/// 组装一帧快照（可重试段）：读 `state` 与当时的 `FOLD_OVERRIDES`，只产出局部
/// `items` 与 `changed_from`，不写任何全局状态。
///
/// 幂等：`state` 侧只有 memoization（`current_turn` 投影缓存、`folded_history`
/// 折叠缓存、todo 摘要/分隔线/分组缓存），重复执行与首次执行结果一致；重试不会
/// 产生非幂等副作用（`#[cfg(test)]` 的 PerfCounter 计数重复可接受）。
///
/// [锁序] 调用方不得持 `VIEW_MODELS` 锁再等别处的 `FOLD_OVERRIDES` 写；本函数
/// 只在内部取 `FOLD_OVERRIDES` 读锁，末尾随 `overrides` 释放。
fn build_publication_snapshot(state: &mut BridgeState) -> (im::Vector<TuiRenderUnit>, usize) {
    // [PERF] 阶段计时（仅测试构建）——见 `PerfCounters::stage_*_ns`；每段末尾
    // 记一次 elapsed 并重置 `__t`，供定向测量按阶段定位成本归属。
    #[cfg(test)]
    let mut __t = peri_time::monotonic_now();
    let mut active = state.current_turn.view_models().clone();
    #[cfg(test)]
    {
        crate::kit::acp_bridge::observe_perf(
            crate::kit::acp_bridge::PerfCounter::StageAssembleNs,
            peri_time::elapsed_since(__t).as_nanos() as u64,
        );
        __t = peri_time::monotonic_now();
    }

    let overrides_state = FOLD_OVERRIDES.state();
    let overrides = overrides_state.read();
    let items = state
        .folded_history
        .project(&state.committed, state.phase, &overrides);

    // [G2] 折叠状态机单点 pass（spec §7 表 + FOLD_OVERRIDES 用户覆盖）。
    // [共享安全] items 通过 join_into 与 current_turn 缓存共享元素——pass 先收集
    // 翻转目标，再用 im::Vector::set（内部 COW）应用，避免就地修改共享节点。
    super::super::fold::apply_fold_pass(&mut active, state.phase, &overrides);
    #[cfg(test)]
    {
        crate::kit::acp_bridge::observe_perf(
            crate::kit::acp_bridge::PerfCounter::StageFoldNs,
            peri_time::elapsed_since(__t).as_nanos() as u64,
        );
        __t = peri_time::monotonic_now();
    }

    let history = items.clone();
    let (mut items, has_history_prompt) = decorated_history(&history);
    let visual_active_start = items.len();
    join_into(&mut items, active);
    insert_turn_dividers(&mut items, visual_active_start, has_history_prompt);
    #[cfg(test)]
    {
        crate::kit::acp_bridge::observe_perf(
            crate::kit::acp_bridge::PerfCounter::StageAssembleNs,
            peri_time::elapsed_since(__t).as_nanos() as u64,
        );
        __t = peri_time::monotonic_now();
    }

    // [§6.9] todo 进度摘要：活动 turn（current_turn 非空）且 TODO_ITEMS 非空时，
    // 插在 trailing 最终回答之前（回答后无 todo）；无 trailing 回答时位于 turn 底部。
    insert_todo_summary(&mut items, state.phase);
    #[cfg(test)]
    {
        crate::kit::acp_bridge::observe_perf(
            crate::kit::acp_bridge::PerfCounter::StageTodoNs,
            peri_time::elapsed_since(__t).as_nanos() as u64,
        );
        __t = peri_time::monotonic_now();
    }

    // [阅读效率] 正文空档分组：正文段落之间的所有过程单元（工具 / thinking /
    // 通知等）合成一张折叠组，默认一组一行；点击该行展开整组。分组是纯视觉
    // 变换，作用于完整快照（committed + current_turn），用户气泡与 turn 分隔线
    // 是天然边界，不会跨 turn 合并。
    // `changed_from` 取分组输出的稳定前缀长度：`[0, changed_from)` 的槽位内容
    // 未变，Transcript 可保留其渲染缓存。
    let detail_hashes = detail_target_hashes();
    let changed_from = group_body_gaps(
        &mut items,
        &overrides,
        &detail_hashes,
        active_turn_of(state),
    );
    drop(overrides);

    (items, changed_from)
}

/// 提交一帧快照：调用方必须持 `VIEW_MODELS` 写锁。
///
/// `previous_generation` 是**成功那次构建的起点** generation——交给 Transcript
/// 判断能否走增量。构建期间若发生手动折叠，点击已推进过 generation 且当帧快照
/// 自带新折叠，Transcript 的上一帧 generation 与 `previous_generation` 不衔接，
/// 会走整段重建（安全行为）；这里不伪造 adjacency。
fn commit_publication(
    state: &mut BridgeState,
    published: &mut ViewModelsSnapshot,
    items: im::Vector<TuiRenderUnit>,
    changed_from: usize,
    previous_generation: u64,
) {
    #[cfg(test)]
    let __t = peri_time::monotonic_now();
    state.generation = state.generation.max(published.generation).wrapping_add(1);
    #[cfg(test)]
    crate::kit::acp_bridge::observe_publication(crate::kit::acp_bridge::PublicationObservation {
        generation: state.generation,
        source_version: state
            .current_turn
            .text
            .len()
            .saturating_add(state.current_turn.reasoning.len()) as u64,
        reason: if state.phase == SessionPhase::PromptRunning {
            crate::kit::acp_bridge::PublicationReason::Intermediate
        } else {
            crate::kit::acp_bridge::PublicationReason::Terminal
        },
    });
    *crate::kit::atoms::TRANSCRIPT_PUBLICATION.state().write() =
        crate::kit::atoms::TranscriptPublication {
            generation: state.generation,
            previous_generation,
            changed_from,
            prepend_epoch: crate::kit::atoms::HISTORY_PREPEND_EPOCH.get(),
            structural_epoch: crate::kit::atoms::STRUCTURAL_EPOCH.get(),
        };
    let snapshot = ViewModelsSnapshot {
        items,
        generation: state.generation,
    };
    *published = snapshot;
    #[cfg(test)]
    crate::kit::acp_bridge::observe_perf(
        crate::kit::acp_bridge::PerfCounter::StageWriteNs,
        peri_time::elapsed_since(__t).as_nanos() as u64,
    );
}

// ---------------------------------------------------------------------------
// [测试] 发布 / 点击交错点
// ---------------------------------------------------------------------------

/// 测试探针：在「快照构建完成、尚未取 `VIEW_MODELS` 写锁」处触发，用于确定性
/// 复现点击与发布的交错（生产构建不编译）。
///
/// 探针只在当次发布的 `(session_id, request_id)` 与挂载时一致时触发，避免同进程
/// 并发运行的其他测试的发布消费掉它。
#[cfg(test)]
pub(crate) struct PublicationInterleaveHook {
    session_id: String,
    request_id: Option<String>,
    probe: std::sync::Arc<dyn Fn() + Send + Sync>,
}

#[cfg(test)]
static PUBLICATION_INTERLEAVE_HOOK: Mutex<Option<PublicationInterleaveHook>> = Mutex::new(None);

/// 挂载交错探针：每次快照构建后调用一次，调用方自行决定是否只模拟一次点击
/// （重试路径会多次进入）。测试收尾须 [`disarm_publication_interleave_hook`]。
#[cfg(test)]
pub(crate) fn arm_publication_interleave_hook(
    state: &BridgeState,
    probe: impl Fn() + Send + Sync + 'static,
) {
    *PUBLICATION_INTERLEAVE_HOOK.lock() = Some(PublicationInterleaveHook {
        session_id: state.active_session_id.clone(),
        request_id: state.current_request_id.clone(),
        probe: std::sync::Arc::new(probe),
    });
}

/// 卸载交错探针（测试收尾——探针不跨用例残留）。
#[cfg(test)]
pub(crate) fn disarm_publication_interleave_hook() {
    *PUBLICATION_INTERLEAVE_HOOK.lock() = None;
}

#[cfg(test)]
fn run_publication_interleave_hook(state: &BridgeState) {
    let probe = {
        let hook = PUBLICATION_INTERLEAVE_HOOK.lock();
        hook.as_ref()
            .filter(|hook| {
                hook.session_id == state.active_session_id
                    && hook.request_id == state.current_request_id
            })
            .map(|hook| std::sync::Arc::clone(&hook.probe))
    };
    // 探针在锁外执行：它会取 VIEW_MODELS 写锁（模拟点击）。
    if let Some(probe) = probe {
        probe();
    }
}

/// [PERF] insert_todo_summary 摘要文本缓存——键 = (TODO_ITEMS 指纹, LANG_VERSION)。
/// 普通文本 token 复用上次摘要文本，跳过 clone + 双扫描 + i18n 格式化。
/// 纯 memoization：文本只依赖 (todos 内容, 语言版本)，命中即与重建等价。
pub(super) static TODO_SUMMARY_CACHE: Mutex<Option<(u64, u64, String)>> = Mutex::new(None);

pub(super) struct DividerHistoryCache {
    pub(super) raw: im::Vector<TuiRenderUnit>,
    pub(super) decorated: im::Vector<TuiRenderUnit>,
    pub(super) has_prompt: bool,
}

pub(super) static DIVIDER_HISTORY: Mutex<Option<DividerHistoryCache>> = Mutex::new(None);

#[must_use]
pub(super) fn is_visible_user_prompt(vm: &TuiRenderUnit) -> bool {
    matches!(vm, TuiRenderUnit::TuiUserBubble(user) if !user.text.is_empty() || !user.attachments.is_empty())
}

/// History is immutable during a streaming turn. Reuse its decorated tree so
/// each token only inserts dividers for newly appended active entries.
pub(super) fn decorated_history(
    history: &im::Vector<TuiRenderUnit>,
) -> (im::Vector<TuiRenderUnit>, bool) {
    let mut cache = DIVIDER_HISTORY.lock();
    if let Some(cached) = cache.as_ref()
        && (cached.raw.ptr_eq(history) || (history.is_inline() && &cached.raw == history))
    {
        return (cached.decorated.clone(), cached.has_prompt);
    }
    let mut decorated = history.clone();
    let has_prompt = if let Some((cached, end)) = cache.as_ref().and_then(|cached| {
        crate::kit::tui_render_unit::prepended_len(&cached.raw, history).map(|end| (cached, end))
    }) {
        decorated.split_off(end);
        let earlier_prompt = insert_turn_dividers(&mut decorated, 0, false);
        let mut old = cached.decorated.clone();
        if earlier_prompt
            && cached.has_prompt
            && let Some(first) = old.iter().position(is_visible_user_prompt)
        {
            let suffix = old.split_off(first);
            decorated.append(old);
            decorated.push_back(TuiRenderUnit::TuiDivider(TuiDivider {
                label: None,
                content_hash: 0,
            }));
            old = suffix;
        }
        decorated = crate::kit::tui_render_unit::prepend_units(decorated, old);
        earlier_prompt || cached.has_prompt
    } else {
        insert_turn_dividers(&mut decorated, 0, false)
    };
    if history.len() > 1024 {
        let mut seen = crate::kit::retained_bytes::SeenAllocations::default();
        let bytes = crate::kit::retained_bytes::retained_units_bytes(history.iter(), &mut seen);
        if bytes > 6 * 1024 * 1024 {
            *cache = None;
            return (decorated, has_prompt);
        }
    }
    *cache = Some(DividerHistoryCache {
        raw: history.clone(),
        decorated: decorated.clone(),
        has_prompt,
    });
    (decorated, has_prompt)
}

/// Put the rule immediately before every user prompt after the first one.
/// This projection depends only on transcript order, so completion and replay
/// cannot remove a rule that was already visible. Keep `committed` free of
/// presentation rows, since cancellation and replay address its real messages.
pub(super) fn insert_turn_dividers(
    items: &mut im::Vector<TuiRenderUnit>,
    start: usize,
    mut seen_prompt: bool,
) -> bool {
    let mut index = start;
    while index < items.len() {
        if is_visible_user_prompt(&items[index]) {
            if seen_prompt {
                items.insert(
                    index,
                    TuiRenderUnit::TuiDivider(TuiDivider {
                        label: None,
                        content_hash: 0,
                    }),
                );
                index += 1;
            }
            seen_prompt = true;
        }
        index += 1;
    }
    seen_prompt
}

/// [§6.9] 从 `TODO_ITEMS` 派生活动 turn 的 todo 进度摘要行。
///
/// 摘要格式 `3/7 tasks · Running tests`：完成数/总数 + 首个 in-progress 项内容。
/// 插在 trailing assistant 回答（最终回答）之前；无 trailing 回答时追加在 turn 底部。
/// turn 结束后 current_turn 清空 → 摘要随快照消失（回答后无 todo）。
pub(super) fn insert_todo_summary(items: &mut im::Vector<TuiRenderUnit>, phase: SessionPhase) {
    use crate::kit::message_area::TodoStatus;
    if phase != SessionPhase::PromptRunning {
        return;
    }
    // 读引用代替 clone——无克隆只读扫描（hash 公式与 footer::hash_todo_items 一致）。
    let todos_state = crate::kit::atoms::TODO_ITEMS.state();
    let todos = todos_state.read();
    if todos.is_empty() {
        return;
    }
    let todos_hash = crate::kit::message_area::hash_todo_items(&todos);
    let lang_version = crate::kit::atoms::LANG_VERSION.get();
    // [PERF] 内容或语言版本变化才重建摘要行；普通 token 复用缓存文本。
    let text = {
        let mut cache = TODO_SUMMARY_CACHE.lock();
        if let Some((h, v, t)) = cache.as_ref()
            && *h == todos_hash
            && *v == lang_version
        {
            t.clone()
        } else {
            let total = todos.len();
            let done = todos
                .iter()
                .filter(|t| t.status == TodoStatus::Completed)
                .count();
            let active = todos
                .iter()
                .find(|t| t.status == TodoStatus::InProgress)
                .map(|t| t.content.clone());
            let text = match active {
                Some(a) => i18n::tr_args(
                    "render-todo-summary-active",
                    &[
                        ("done".to_string(), FluentValue::from(done as u64)),
                        ("total".to_string(), FluentValue::from(total as u64)),
                        ("active".to_string(), FluentValue::from(a)),
                    ],
                ),
                None => i18n::tr_args(
                    "render-todo-summary",
                    &[
                        ("done".to_string(), FluentValue::from(done as u64)),
                        ("total".to_string(), FluentValue::from(total as u64)),
                    ],
                ),
            };
            *cache = Some((todos_hash, lang_version, text.clone()));
            text
        }
    };
    let summary = TuiRenderUnit::TuiTodoSummary(TuiTodoSummary::new(text));
    // trailing 最终回答 = 含正文的 assistant bubble（流式或已冻结的 turn 尾部）。
    // 纯 reasoning 气泡在分组侧是过程单元（`is_body_unit` 判 text 为空），若把它当
    // 最终回答，todo 会插到它前面，把末尾过程段切成两段（尾部单独成组）。
    let is_trailing_answer = matches!(
        items.back(),
        Some(TuiRenderUnit::TuiAssistantBubble(b)) if !b.text.is_empty()
    );
    let insert_at = if is_trailing_answer {
        items.len() - 1
    } else {
        items.len()
    };
    items.insert(insert_at, summary);
}

// ---------------------------------------------------------------------------
// [阅读效率] 正文空档分组
// ---------------------------------------------------------------------------

/// 正文单元（分组边界）：用户气泡、含正文的 assistant 气泡、turn 分隔线、
/// todo 摘要与交互块。
///
/// 子 Agent 组是可交互的 durable entry（聚焦后 Enter 打开详情），不参与自动
/// 隐藏；纯 reasoning 气泡（无正文）与工具 / 普通通知 / info 级提醒属于过程单元。
/// [§15] 错误级通知与 required / warning 及以上的提醒保持可见，不折进组行。
///
/// 详情目标（超大正文占位 note、外置正文的工具卡）同样保持可见：组行只暴露
/// 一个槽位，组内多个目标无法分别打开详情，分组不能把它们藏到同一槽位后面。
#[must_use]
pub(super) fn is_body_unit(
    vm: &TuiRenderUnit,
    detail_hashes: &std::collections::HashSet<u64>,
) -> bool {
    if detail_hashes.contains(&vm.content_hash()) {
        return true;
    }
    match vm {
        TuiRenderUnit::TuiAssistantBubble(b) => !b.text.is_empty(),
        TuiRenderUnit::TuiSystemNote(note) => note.level == TuiNoteLevel::Error,
        TuiRenderUnit::TuiSystemReminder(reminder) => {
            reminder.required
                || reminder.severity >= peri_acp_types::system_reminder::ReminderSeverity::Warning
        }
        TuiRenderUnit::TuiUserBubble(_)
        | TuiRenderUnit::TuiDivider(_)
        | TuiRenderUnit::TuiTodoSummary(_)
        | TuiRenderUnit::TuiAskUserBlock(_)
        | TuiRenderUnit::TuiSubAgentGroup(_) => true,
        _ => false,
    }
}

/// 组身份——取组首成员的稳定身份：流式追加只在组尾增员，键不变，用户展开
/// 选择与渲染缓存键因此跨重建稳定。
fn group_key_of(vm: &TuiRenderUnit) -> String {
    match vm {
        TuiRenderUnit::TuiToolCard(t) => format!("tool:{}", t.tool_id),
        TuiRenderUnit::TuiAssistantBubble(b) => match b.message_id.as_deref() {
            Some(id) => format!("reasoning:{id}"),
            // 兼容无身份数据：内容身份可跨重建稳定，仅极端内容冲突会共享折叠态。
            None => format!("reasoning#{:016x}", b.content_hash),
        },
        TuiRenderUnit::TuiSystemNote(note) => format!("note#{:016x}", note.content_hash),
        TuiRenderUnit::TuiSystemReminder(reminder) => format!("reminder:{}", reminder.reminder_id),
        other => format!("unit#{:016x}", other.content_hash()),
    }
}

/// 组内失败单元：error 工具与 error 级通知——组行摘要保留失败信号。
fn is_failed_unit(vm: &TuiRenderUnit) -> bool {
    match vm {
        TuiRenderUnit::TuiToolCard(t) => t.is_error,
        TuiRenderUnit::TuiSystemNote(note) => note.level == TuiNoteLevel::Error,
        _ => false,
    }
}

/// 组行摘要：成员类型统计（`Read 2 · Thought 1`）。
///
/// 运行态不再拼 `running` 后缀——尾部工作组由组行的 `Working` 状态标签表达
/// （见 `mark_working_group`），避免同一组出现两个状态词。
fn group_title(run: &[TuiRenderUnit]) -> String {
    let mut parts: Vec<(String, u32)> = Vec::new();
    for vm in run {
        let label = match vm {
            TuiRenderUnit::TuiToolCard(t) => {
                Some(crate::kit::tool_display::format_tool_name(&t.tool_name))
            }
            TuiRenderUnit::TuiAssistantBubble(b) if b.reasoning.is_some() => {
                Some(i18n::tr("render-group-thought"))
            }
            TuiRenderUnit::TuiSystemNote(_) => Some(i18n::tr("render-group-note")),
            TuiRenderUnit::TuiSystemReminder(_) => Some(i18n::tr("render-group-reminder")),
            _ => None,
        };
        let Some(label) = label else { continue };
        match parts.iter_mut().find(|(name, _)| *name == label) {
            Some((_, count)) => *count += 1,
            None => parts.push((label, 1)),
        }
    }
    let mut title = parts
        .into_iter()
        .map(|(name, count)| {
            if count > 1 {
                format!("{name} \u{d7}{count}")
            } else {
                name
            }
        })
        .collect::<Vec<_>>()
        .join(" \u{b7} ");
    if title.is_empty() {
        title = i18n::tr_args(
            "render-group-steps",
            &[("count".to_string(), FluentValue::from(run.len()))],
        );
    }
    title
}

/// 由一段连续过程单元构造折叠组。
fn build_collapsed_group(
    run: Vec<TuiRenderUnit>,
    overrides: &std::collections::HashMap<FoldKey, FoldState>,
) -> TuiRenderUnit {
    let key = group_key_of(&run[0]);
    let failed_count = run.iter().filter(|vm| is_failed_unit(vm)).count() as u32;
    let fold = overrides
        .get(&FoldKey::Group(key.clone()))
        .copied()
        .unwrap_or(FoldState::Collapsed);
    let mut group = TuiCollapsedGroup {
        key,
        title: group_title(&run),
        count: run.len() as u32,
        failed_count,
        // 成员保留折叠 pass 给出的 fold（默认 Collapsed，FOLD_OVERRIDES 优先）：
        // 组行只折叠一层，展开后每个成员仍是自己的折叠态、可单独点开。
        view_models: run,
        fold,
        is_working: false,
        content_hash: 0,
    };
    group.recompute_hash();
    TuiRenderUnit::TuiCollapsedGroup(group)
}

/// 分组辅助输入指纹——与快照内容无关但会改变分组结果的输入：语言版本
/// （组摘要 i18n 标签）、组折叠覆盖（用户手动展开/收起）、详情目标集合
/// （详情目标单元必须保持独立槽位）与 `active_turn`（尾组 Working 标记）。
fn body_gap_aux_fingerprint(
    overrides: &std::collections::HashMap<FoldKey, FoldState>,
    detail_hashes: &std::collections::HashSet<u64>,
    active_turn: bool,
) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut group_overrides: Vec<(&str, u64)> = overrides
        .iter()
        .filter_map(|(key, fold)| match key {
            FoldKey::Group(id) => Some((
                id.as_str(),
                crate::kit::tui_render_unit::fold_state_code(*fold),
            )),
            _ => None,
        })
        .collect();
    group_overrides.sort_unstable();
    let mut details: Vec<u64> = detail_hashes.iter().copied().collect();
    details.sort_unstable();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    group_overrides.hash(&mut hasher);
    details.hash(&mut hasher);
    // active_turn 翻转（TurnDone / 中断 / 历史回放）必须整体重建分组缓存，
    // 否则输入逐条相同会让上一帧的尾组标记与 Working 缓存一起留下。
    active_turn.hash(&mut hasher);
    crate::kit::tui_render_unit::tui_hash_combine(
        crate::kit::atoms::LANG_VERSION.get(),
        hasher.finish(),
    )
}

/// 本 turn 是否正在产出：prompt 运行中且 `current_turn` 非空。
///
/// `current_turn` 为空时不点亮——新一轮 prompt 刚提交、用户气泡尚未到达时，
/// 上一轮遗留的尾组不应重新亮起。
fn active_turn_of(state: &BridgeState) -> bool {
    state.phase == SessionPhase::PromptRunning && !state.current_turn.is_empty()
}

/// 当前可打开详情的目标内容哈希——这些单元各自保留独立槽位。
fn detail_target_hashes() -> std::collections::HashSet<u64> {
    crate::kit::atoms::HISTORY_DETAIL_TARGETS
        .state()
        .read()
        .iter()
        .map(|target| target.content_hash)
        .collect()
}

/// 稳定切点（输入下标升序）：输出 `[0, output)` 只依赖输入 `[0, input)`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct GroupCut {
    pub(super) input: usize,
    pub(super) output: usize,
    pub(super) anchor: GroupCutAnchor,
}

/// 切点的稳定依据。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GroupCutAnchor {
    /// 前缀以正文单元收口（或空前缀）——组不能跨过正文，恒稳定。
    AfterBody,
    /// 前缀以完整折叠组收口——切点后的单元若仍是非正文，组会继续并入，
    /// 须按新输入重判。
    AfterGroup,
}

pub(super) const GROUP_CUT_CAP: usize = 512;

#[derive(Debug, Clone)]
pub(super) struct BodyGapGroupCache {
    input: im::Vector<TuiRenderUnit>,
    grouped: im::Vector<TuiRenderUnit>,
    cuts: Vec<GroupCut>,
    aux: u64,
}

/// [PERF] 正文空档分组结果缓存（纯函数 memoization）。
///
/// 复用判定：逐条比较输入找出首个差异下标 `d`，取 `input <= d` 的最大稳定
/// 切点复用输出前缀，只重建变化后缀——成本 ∝ 变化量，不随历史长度增长。
pub(super) static BODY_GAP_GROUP_CACHE: Mutex<Option<BodyGapGroupCache>> = Mutex::new(None);

/// 清空分组缓存（测试用——全局单例缓存会跨用例残留）。
#[cfg(test)]
pub(crate) fn reset_body_gap_group_cache() {
    *BODY_GAP_GROUP_CACHE.lock() = None;
}

/// 切点在新输入下是否仍稳定（见 [`GroupCutAnchor`]）。
fn cut_stable(
    anchor: GroupCutAnchor,
    input: &im::Vector<TuiRenderUnit>,
    cut: usize,
    detail_hashes: &std::collections::HashSet<u64>,
) -> bool {
    match anchor {
        GroupCutAnchor::AfterBody => true,
        GroupCutAnchor::AfterGroup => match input.get(cut) {
            None => true,
            Some(vm) => is_body_unit(vm, detail_hashes),
        },
    }
}

/// 正文空档分组：把每个「正文段落之间的连续过程单元」替换为一张折叠组。
///
/// 返回分组输出的稳定前缀长度（`changed_from`）：`[0, changed_from)` 的槽位
/// 内容与上一帧一致，Transcript 可保留其渲染缓存。
pub(super) fn group_body_gaps(
    items: &mut im::Vector<TuiRenderUnit>,
    overrides: &std::collections::HashMap<FoldKey, FoldState>,
    detail_hashes: &std::collections::HashSet<u64>,
    active_turn: bool,
) -> usize {
    let input = items.clone();
    let aux = body_gap_aux_fingerprint(overrides, detail_hashes, active_turn);
    let mut cache = BODY_GAP_GROUP_CACHE.lock();
    let (rebuild_from, mut grouped, mut cuts) = match cache.as_ref() {
        Some(cached) if cached.aux == aux => {
            let common = cached.input.len().min(input.len());
            let d = (0..common)
                .find(|&i| !same_render_unit(&cached.input[i], &input[i]))
                .unwrap_or(common);
            if d == input.len() && d == cached.input.len() {
                // 输入逐条一致——整段复用（O(1) 引用计数）。
                let grouped = cached.grouped.clone();
                drop(cache);
                *items = grouped;
                return items.len();
            }
            if input.len() > cached.input.len()
                && d < common
                && let Some((mut grouped, cuts)) =
                    group_prepended_page(cached, &input, overrides, detail_hashes)
            {
                mark_working_group(&mut grouped, 0, active_turn);
                *items = grouped.clone();
                *cache = Some(BodyGapGroupCache {
                    input,
                    grouped,
                    cuts,
                    aux,
                });
                return 0;
            }
            match cached
                .cuts
                .iter()
                .rev()
                .filter(|cut| cut.input <= d)
                .find(|cut| cut_stable(cut.anchor, &input, cut.input, detail_hashes))
            {
                Some(cut) => {
                    let mut prefix = cached.grouped.clone();
                    prefix.split_off(cut.output);
                    let kept = cached
                        .cuts
                        .iter()
                        .copied()
                        .take_while(|c| c.input <= cut.input)
                        .collect();
                    (cut.input, prefix, kept)
                }
                None => (0, im::Vector::new(), Vec::new()),
            }
        }
        _ => (0, im::Vector::new(), Vec::new()),
    };
    drop(cache);
    let changed_from = grouped.len();
    let suffix = if rebuild_from == 0 {
        input.clone()
    } else {
        input.clone().split_off(rebuild_from)
    };
    rebuild_grouped_suffix(
        suffix,
        rebuild_from,
        &mut grouped,
        &mut cuts,
        overrides,
        detail_hashes,
    );
    if cuts.len() > GROUP_CUT_CAP {
        cuts.drain(0..cuts.len() - GROUP_CUT_CAP);
    }
    let changed_from = mark_working_group(&mut grouped, changed_from, active_turn);
    *BODY_GAP_GROUP_CACHE.lock() = Some(BodyGapGroupCache {
        input,
        grouped: grouped.clone(),
        cuts,
        aux,
    });
    *items = grouped;
    changed_from.min(items.len())
}

/// 从尾部向前跳过 todo 摘要，返回最后一张非正文单元的折叠组下标。
///
/// todo 摘要是正文单元，可能追加在 turn 底部（§6.9），但不应遮挡尾组。
fn tail_group_index(grouped: &im::Vector<TuiRenderUnit>) -> Option<usize> {
    grouped
        .iter()
        .enumerate()
        .rev()
        .find(|(_, unit)| !matches!(unit, TuiRenderUnit::TuiTodoSummary(_)))
        .and_then(|(idx, unit)| matches!(unit, TuiRenderUnit::TuiCollapsedGroup(_)).then_some(idx))
}

/// `[0, changed_from)` 前缀里上一帧还被标记为 Working 的组（同样跳过尾部 todo，
/// 前缀末尾可能是 `[…, 旧组, todo]`）。
fn stale_working_index(grouped: &im::Vector<TuiRenderUnit>, changed_from: usize) -> Option<usize> {
    (0..changed_from.min(grouped.len()))
        .rev()
        .find(|&idx| !matches!(&grouped[idx], TuiRenderUnit::TuiTodoSummary(_)))
        .filter(|&idx| {
            matches!(&grouped[idx], TuiRenderUnit::TuiCollapsedGroup(group) if group.is_working)
        })
}

fn set_group_working(grouped: &mut im::Vector<TuiRenderUnit>, idx: usize, working: bool) {
    let Some(TuiRenderUnit::TuiCollapsedGroup(group)) = grouped.get_mut(idx) else {
        return;
    };
    if group.is_working == working {
        return;
    }
    group.is_working = working;
    group.recompute_hash();
}

/// 标记尾组为 Working，并清掉被新正文挤下去的旧尾组标记。
///
/// 旧组必须清除且把 `changed_from` 下移到它的下标：它已不在 Transcript 的
/// 重扫区间里，否则该 slot 会保留最后一帧的扫光。
fn mark_working_group(
    grouped: &mut im::Vector<TuiRenderUnit>,
    changed_from: usize,
    active_turn: bool,
) -> usize {
    let active_idx = if active_turn {
        tail_group_index(grouped)
    } else {
        None
    };
    if let Some(idx) = active_idx {
        set_group_working(grouped, idx, true);
    }
    let Some(stale_idx) =
        stale_working_index(grouped, changed_from).filter(|&idx| Some(idx) != active_idx)
    else {
        return changed_from;
    };
    set_group_working(grouped, stale_idx, false);
    changed_from.min(stale_idx)
}

/// [PERF] 把 `tail` 并入 `items`——按尾长选择代价更低的合并方式。
///
/// `im::Vector::append` 在「大向量接小向量」路径上**退化为整树重建**：实测
/// （release，`TuiRenderUnit` 320 B）base=1000 时并入 1 个元素耗时 157 µs、
/// 并入 256 个元素 186 µs，成本 ∝ `items.len()`；同等条件下逐元素 `push_back`
/// 只用 5.2 / 64 µs（只复制右脊与右端 chunk）。快照组装（committed 接
/// current_turn）正是「大主体接小尾巴」，故尾部明显更小时走 `push_back`；
/// 两者量级相当时仍交给 im 的树合并（尾长 ≥ 主体一半时树合并更省）。
///
/// 纯搬运：元素顺序与内容不变。`tail` 按值传入——独占来源（`split_off` /
/// `slice` 的结果）直接移动零复制；共享来源（如 `current_turn` 缓存）需先
/// `clone()`（O(1) 引用计数），由 im 在推入时按 chunk 承担 COW。
pub(super) fn join_into(items: &mut im::Vector<TuiRenderUnit>, tail: im::Vector<TuiRenderUnit>) {
    if tail.is_empty() {
        return;
    }
    if tail.len() * 2 <= items.len() {
        for unit in tail {
            items.push_back(unit);
        }
        return;
    }
    items.append(tail);
}

/// 结构性释放：丢弃所有持有旧 `committed` 节点的投影缓存。
pub(crate) fn invalidate_structural_caches() {
    *DIVIDER_HISTORY.lock() = None;
    *TODO_SUMMARY_CACHE.lock() = None;
    *BODY_GAP_GROUP_CACHE.lock() = None;
}

#[cfg(test)]
pub(crate) fn debug_cache_lengths() -> (Option<usize>, Option<usize>) {
    (
        BODY_GAP_GROUP_CACHE
            .lock()
            .as_ref()
            .map(|cache| cache.input.len()),
        DIVIDER_HISTORY.lock().as_ref().map(|cache| cache.raw.len()),
    )
}

/// 由 acp_bridge 在 BRIDGE_RESET_COUNTER 复位时调用——
/// 立即将空快照写入 VIEW_MODELS atom，防止其他 reader 读到旧 session 数据。
pub fn push_view_models_for_reset() {
    #[cfg(test)]
    crate::kit::acp_bridge::observe_publication(crate::kit::acp_bridge::PublicationObservation {
        generation: 0,
        source_version: 0,
        reason: crate::kit::acp_bridge::PublicationReason::Reset,
    });
    // [Slice 2] session 复位时清空折叠覆盖表——tool_id/message_id/agent_id/
    // reminder_id 跨 session 不保证唯一，残留覆盖会错误作用于新会话的同名 entry。
    crate::kit::fold_overrides::clear();
    // [S2 §3.4] 焦点单一事实源同源清空（跨 session 身份不唯一——slot 与 key
    // 都依赖旧会话索引/身份，残留会让新会话焦点/免疫错误指向）。
    *crate::kit::atoms::FOCUSED_ENTRY.state().write() = None;
    invalidate_structural_caches();
    let snapshot = ViewModelsSnapshot {
        items: im::Vector::new(),
        generation: 0,
    };
    let view_models = VIEW_MODELS.state();
    let mut published_snapshot = view_models.write();
    *crate::kit::atoms::TRANSCRIPT_PUBLICATION.state().write() =
        crate::kit::atoms::TranscriptPublication {
            prepend_epoch: crate::kit::atoms::HISTORY_PREPEND_EPOCH.get(),
            structural_epoch: crate::kit::atoms::STRUCTURAL_EPOCH.get(),
            ..Default::default()
        };
    *published_snapshot = snapshot;
    crate::kit::message_area::history_layout::reset();
}

use crate::kit::atoms::FOLD_OVERRIDES;
