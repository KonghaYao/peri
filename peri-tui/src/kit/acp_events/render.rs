//! Render helper functions — push_view_models, push_acp_state, etc.

use super::*;
use crate::i18n;
use crate::kit::atoms::FOLD_OVERRIDES;
use crate::kit::submit_request::SubmitRequest;
use crate::kit::tui_render_unit::{
    EntryStatus, FoldKey, FoldState, FoldTarget, TuiDivider, TuiRenderUnit, TuiTodoSummary,
    TuiToolCard, TuiToolPresentation, fold_for_status, fold_state_code, tui_hash_combine,
};
use fluent_bundle::FluentValue;
use std::sync::Mutex;

/// 将 BridgeState 中的 ViewModels 写入 VIEW_MODELS Atom。
///
/// 从 `state.committed`（im::Vector）clone（O(1)引用计数）后并入
/// `current_turn.view_models()`（见 [`join_into`]：按尾长选择 `push_back` 或树合并，
/// 与 current_turn 增量缓存共享元素，不逐条深拷贝），构成扁平单层列表。
/// generation 每次调用递增+1。
///
/// 快照后处理流水线（均为纯视觉变换，不触碰 segment↔cache 对齐）：
/// 1. turn 边界 `TuiDivider`（§6.6：committed 与 current_turn 之间）；
/// 2. `apply_fold_pass`（spec §7 表 + FOLD_OVERRIDES）；
/// 3. todo 进度摘要行（§6.9：TODO_ITEMS 派生，插在最终回答前）；
/// 4. `group_successful_tools`（§7：相邻成功工具压成 `TuiCollapsedGroup`）。
pub(crate) fn push_view_models(state: &mut BridgeState) {
    // [Diagnostic] 追踪 VIEW_MODELS 写入时机——配合 scroll diag 分析 submit/history 滚动问题。
    // trace 级别：每 token 调用一次，默认 info filter 下不落盘。
    let is_loading = state.phase == SessionPhase::PromptRunning;
    tracing::trace!(
        target: "msg_scroll_diag",
        committed = state.committed.len(),
        current_turn = state.current_turn.view_models().len(),
        generation = state.generation,
        phase = ?state.phase,
        is_loading,
        "push_view_models: writing VIEW_MODELS atom",
    );
    // [PERF] 阶段计时（仅测试构建）——见 `PerfCounters::stage_*_ns`；每段末尾
    // 记一次 elapsed 并重置 `__t`，供定向测量按阶段定位成本归属。
    #[cfg(test)]
    let mut __t = std::time::Instant::now();
    let mut items = state.committed.clone();

    // [§6.6] turn 边界 divider：committed 末尾是**新 turn 的用户 prompt**（≥2 项，
    // 说明存在上一 turn 内容）且 current_turn 有内容时，在 prompt 之前插一条
    // 无 label 分隔线——committed|current_turn 边界本身通常是「prompt ↔ 回复」
    // 的同一 turn 内部，不能直接用它；以「末项为 user bubble」判定新 turn 起点。
    if items.len() >= 2
        && matches!(items.back(), Some(TuiRenderUnit::TuiUserBubble(_)))
        && !state.current_turn.is_empty()
        && !matches!(
            items.get(items.len() - 2),
            Some(TuiRenderUnit::TuiDivider(_))
        )
    {
        items.insert(
            items.len() - 1,
            TuiRenderUnit::TuiDivider(TuiDivider {
                label: None,
                content_hash: 0,
            }),
        );
    }
    join_into(&mut items, state.current_turn.view_models().clone());
    #[cfg(test)]
    {
        crate::kit::acp_bridge::observe_perf(
            crate::kit::acp_bridge::PerfCounter::StageAssembleNs,
            __t.elapsed().as_nanos() as u64,
        );
        __t = std::time::Instant::now();
    }

    // [G2] 折叠状态机单点 pass（spec §7 表 + FOLD_OVERRIDES 用户覆盖）。
    // [共享安全] items 通过 join_into 与 current_turn 缓存共享元素——pass 先收集
    // 翻转目标，再用 im::Vector::set（内部 COW）应用，避免就地修改共享节点。
    apply_fold_pass(&mut items, state.phase);
    #[cfg(test)]
    {
        crate::kit::acp_bridge::observe_perf(
            crate::kit::acp_bridge::PerfCounter::StageFoldNs,
            __t.elapsed().as_nanos() as u64,
        );
        __t = std::time::Instant::now();
    }

    // [§6.9] todo 进度摘要：活动 turn（current_turn 非空）且 TODO_ITEMS 非空时，
    // 插在 trailing 最终回答之前（回答后无 todo）；无 trailing 回答时位于 turn 底部。
    insert_todo_summary(&mut items, state.phase);
    #[cfg(test)]
    {
        crate::kit::acp_bridge::observe_perf(
            crate::kit::acp_bridge::PerfCounter::StageTodoNs,
            __t.elapsed().as_nanos() as u64,
        );
        __t = std::time::Instant::now();
    }

    // [§7] 相邻成功工具分组——作用于完整 snapshot。TurnDone 会先把 current_turn
    // 搬入 committed 再发布终态快照；若只扫描 current_turn 段，归档边界会让同一批
    // 工具从 TuiCollapsedGroup 退回独立卡片。用户气泡、divider、文本及不可分组工具
    // 仍作为天然边界，因此完整扫描不会跨 turn 合并。
    group_successful_tools(&mut items, 0);
    #[cfg(test)]
    {
        crate::kit::acp_bridge::observe_perf(
            crate::kit::acp_bridge::PerfCounter::StageGroupNs,
            __t.elapsed().as_nanos() as u64,
        );
        __t = std::time::Instant::now();
    }

    state.generation = state.generation.wrapping_add(1);
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
    let snapshot = ViewModelsSnapshot {
        items,
        generation: state.generation,
    };
    tracing::trace!(target: "frozen_diag", gen = state.generation, "bridge: acquiring VIEW_MODELS write lock");
    *VIEW_MODELS.state().write() = snapshot;
    #[cfg(test)]
    crate::kit::acp_bridge::observe_perf(
        crate::kit::acp_bridge::PerfCounter::StageWriteNs,
        __t.elapsed().as_nanos() as u64,
    );
    tracing::trace!(target: "frozen_diag", "bridge: wrote VIEW_MODELS");
}

/// [PERF] insert_todo_summary 摘要文本缓存——键 = (TODO_ITEMS 指纹, LANG_VERSION)。
/// 普通文本 token 复用上次摘要文本，跳过 clone + 双扫描 + i18n 格式化。
/// 纯 memoization：文本只依赖 (todos 内容, 语言版本)，命中即与重建等价。
static TODO_SUMMARY_CACHE: Mutex<Option<(u64, u64, String)>> = Mutex::new(None);

/// [§6.9] 从 `TODO_ITEMS` 派生活动 turn 的 todo 进度摘要行。
///
/// 摘要格式 `3/7 tasks · Running tests`：完成数/总数 + 首个 in-progress 项内容。
/// 插在 trailing assistant 回答（最终回答）之前；无 trailing 回答时追加在 turn 底部。
/// turn 结束后 current_turn 清空 → 摘要随快照消失（回答后无 todo）。
fn insert_todo_summary(items: &mut im::Vector<TuiRenderUnit>, phase: SessionPhase) {
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
        let mut cache = TODO_SUMMARY_CACHE.lock().unwrap_or_else(|e| e.into_inner());
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
    // trailing 最终回答 = 末元素 assistant bubble（流式或已冻结的 turn 尾部）
    let is_trailing_answer = matches!(
        items.back(),
        Some(TuiRenderUnit::TuiAssistantBubble(b)) if !b.text.is_empty() || b.reasoning.is_some()
    );
    let insert_at = if is_trailing_answer {
        items.len() - 1
    } else {
        items.len()
    };
    items.insert(insert_at, summary);
}

/// [PERF] group_successful_tools 结果缓存（纯 memoization）。
///
/// 判定分两层：
/// - [`group_aux_fingerprint`] 覆盖「段内容之外」的分组输入（焦点键、Group 覆盖
///   折叠态、语言版本）——变化即全量重建（无法定位影响范围）；
/// - 段内容是否变化由 [`same_entry`] **逐条**比较判定。逐条比较能定位首个差异
///   下标，配合 [`ToolGroupCache::cuts`] 只重建变化后缀——旧实现把整段折成一个
///   u64，任何一处变化都触发全量重建（O(历史) 深拷贝），长会话下每事件成本随
///   历史线性增长。
struct ToolGroupCache {
    /// 上次分组的输入段（O(1) clone，与快照共享节点）。
    input: im::Vector<TuiRenderUnit>,
    /// 上次分组的输出（含合并组）。
    grouped: im::Vector<TuiRenderUnit>,
    /// 稳定切点表（升序），见 [`GroupCut`]。
    cuts: Vec<GroupCut>,
    /// 见 [`group_aux_fingerprint`]。
    aux: u64,
}

/// 稳定切点：输出 `[0, output)` 就是输入 `[0, input)` 的分组结果，且该结果只依赖
/// 前缀内部——复用它等价于对前缀重跑分组（纯函数 memoization，见 [`CutAnchor`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GroupCut {
    input: usize,
    output: usize,
    anchor: CutAnchor,
}

/// 切点的稳定依据。分组是局部判定（相邻可合并连串 / error 失败 lookahead），
/// 故稳定条件可按切点两侧的**相邻条目**给出；复用时前缀内部条目已由
/// [`same_entry`] 逐条比对确认未变，只需按**新输入**重判切点这一处（[`cut_stable`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CutAnchor {
    /// 切点前一条目（`input - 1`）是不可合并、非 error 的普通条目：它既是天然
    /// 边界（其前的可合并连串已收口，不会跨切点续接），也不会替更前的组补记失败
    /// 计数（失败 lookahead 只数 error 条目）。error 条目除外——它是失败 lookahead
    /// 的一部分，其计数取决于切点之后的条目，故按新输入重判。
    AfterCard,
    /// 切点位于折叠组的收口处（`input - 1` 是该组最后一张可合并卡）：组是否已
    /// 封口取决于切点**处**的条目——可合并则会继续并入、error 则会改写组的失败
    /// 计数，两者都须按新输入重判。
    AfterGroup,
}

/// 切点表上限——只需覆盖「近期尾部」的变化，超出后丢弃最旧切点（最坏退化为
/// 从 0 重建，不影响正确性）。
const GROUP_CUT_CAP: usize = 512;

static TOOL_GROUP_CACHE: Mutex<Option<ToolGroupCache>> = Mutex::new(None);

/// 清空分组缓存（测试用——全局单例缓存会跨用例残留）。
#[cfg(test)]
pub(crate) fn reset_tool_group_cache() {
    *TOOL_GROUP_CACHE.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// 单独运行一次分组（测试用）——等价于 `push_view_models` 的 §7 阶段，
/// 供 `acp_events_test/legacy_group_probe_test.rs` 与旧实现做逐条等价对照。
#[cfg(test)]
pub(crate) fn regroup_at_start(items: &mut im::Vector<TuiRenderUnit>) {
    group_successful_tools(items, 0);
}

/// 分组决策的辅助输入指纹——与段内容无关、但会改变分组结果的全局输入：
/// 焦点键（焦点工具免疫——焦点所在 entry 不入组）、Group 覆盖折叠态（用户手动
/// 展开/折叠后不再自动合并）、语言版本（组标题经 `format_tool_name` 生成）。
///
/// `focused` 由调用方传入（同一次 [`FOCUSED_ENTRY`] 读值既供免疫判定、又供指纹），
/// 避免在已持有该 atom 读锁时重入读取。
fn group_aux_fingerprint(focused: Option<&str>) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h: u64 = 0;
    let mut fh = std::collections::hash_map::DefaultHasher::new();
    focused.hash(&mut fh);
    h = tui_hash_combine(h, fh.finish());
    // 分组自身的展开覆盖不会写回源 tool card，因此必须直接参与缓存判定；
    // 否则下一帧会复用旧的 collapsed group，表现为点击/Enter 后不展开。
    let mut oh = std::collections::hash_map::DefaultHasher::new();
    let overrides_state = FOLD_OVERRIDES.state();
    let overrides_guard = overrides_state.read();
    let mut group_overrides: Vec<_> = overrides_guard
        .iter()
        .filter_map(|(key, fold)| match key {
            FoldKey::Group(ids) => Some((ids, fold_state_code(*fold))),
            _ => None,
        })
        .collect();
    group_overrides.sort_by_key(|(ids, _)| *ids);
    group_overrides.hash(&mut oh);
    h = tui_hash_combine(h, oh.finish());
    tui_hash_combine(h, crate::kit::atoms::LANG_VERSION.get())
}

/// 工具卡参与合并决策的字段（值比较；旧整段指纹把它们拼进 hash）。
fn tool_merge_fields(t: &TuiToolCard) -> (&str, &str, u8) {
    let flags = u8::from(t.is_running)
        | (u8::from(t.is_error) << 1)
        | (u8::from(t.diff.is_some()) << 2)
        | (u8::from(t.user_modified) << 3)
        | (u8::from(matches!(t.presentation, TuiToolPresentation::Generic)) << 4);
    (&t.tool_id, &t.tool_name, flags)
}

/// 单条 render unit 的分组身份比较——与旧整段指纹**逐条同口径**（变体码 +
/// content_hash；工具卡显式补充合并决策字段，兜底手造条目 content_hash=0 的
/// 测试场景）。
fn same_entry(a: &TuiRenderUnit, b: &TuiRenderUnit) -> bool {
    if std::mem::discriminant(a) != std::mem::discriminant(b)
        || a.content_hash() != b.content_hash()
    {
        return false;
    }
    match (a, b) {
        (TuiRenderUnit::TuiToolCard(x), TuiRenderUnit::TuiToolCard(y)) => {
            tool_merge_fields(x) == tool_merge_fields(y)
        }
        _ => true,
    }
}

/// 首个差异下标——逐条身份比较；前段完全一致时返回较短段的长度。
///
/// [PERF] 顺序迭代（叶子游标）而非逐下标 `get`：每次 `get` 都要一次树下降，
/// 而本函数在最常见情形（整段前缀一致）会走满全长，是每事件最热的 O(段长) 循环。
fn first_divergence(a: &im::Vector<TuiRenderUnit>, b: &im::Vector<TuiRenderUnit>) -> usize {
    for (i, (x, y)) in a.iter().zip(b.iter()).enumerate() {
        if !same_entry(x, y) {
            return i;
        }
    }
    a.len().min(b.len())
}

/// [PERF] 把 `tail` 并入 `items`——按尾长选择代价更低的合并方式。
///
/// `im::Vector::append` 在「大向量接小向量」路径上**退化为整树重建**：实测
/// （release，`TuiRenderUnit` 320 B）base=1000 时并入 1 个元素耗时 157 µs、
/// 并入 256 个元素 186 µs，成本 ∝ `items.len()`；同等条件下逐元素 `push_back`
/// 只用 5.2 / 64 µs（只复制右脊与右端 chunk）。快照组装（committed 接
/// current_turn）与分组重建（复用前缀接变化后缀）都是「大主体接小尾巴」，
/// 故尾部明显更小时走 `push_back`；两者量级相当时仍交给 im 的树合并
/// （尾长 ≥ 主体一半时树合并更省）。
///
/// 纯搬运：元素顺序与内容不变。`tail` 按值传入——独占来源（`split_off` /
/// `slice` 的结果）直接移动零复制；共享来源（如 `current_turn` 缓存）需先
/// `clone()`（O(1) 引用计数），由 im 在推入时按 chunk 承担 COW。
fn join_into(items: &mut im::Vector<TuiRenderUnit>, tail: im::Vector<TuiRenderUnit>) {
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

/// 切点在**新输入**下是否仍然稳定（见 [`CutAnchor`]）。
fn cut_stable(
    anchor: CutAnchor,
    segment: &im::Vector<TuiRenderUnit>,
    cut: usize,
    focused: Option<&str>,
) -> bool {
    match anchor {
        CutAnchor::AfterCard => match cut.checked_sub(1).and_then(|p| segment.get(p)) {
            // 空前缀：不复用任何内容，恒稳定。
            None => cut == 0,
            Some(prev) if is_mergeable(prev, focused) => false,
            // error 条目的失败计数取决于其后的 error 连串——按新输入重判。
            Some(prev) if is_error_card(prev) => {
                !matches!(segment.get(cut), Some(v) if is_error_card(v))
            }
            Some(_) => true,
        },
        CutAnchor::AfterGroup => match segment.get(cut) {
            // 段尾：可合并连串与失败连串都已收口。
            None => true,
            Some(v) => !is_mergeable(v, focused) && !is_error_card(v),
        },
    }
}

/// 不大于 `d` 的最大稳定切点（按新输入重判，见 [`cut_stable`]）；无则 `None`
/// （须从 0 全量重建）。
fn stable_cut(
    cuts: &[GroupCut],
    segment: &im::Vector<TuiRenderUnit>,
    d: usize,
    focused: Option<&str>,
) -> Option<(usize, usize)> {
    cuts.iter()
        .rev()
        .filter(|c| c.input <= d)
        .find(|c| cut_stable(c.anchor, segment, c.input, focused))
        .map(|c| (c.input, c.output))
}

/// 条目是否可并入折叠组——与 §7 表口径一致（见 [`group_successful_tools`]）。
fn is_mergeable(vm: &TuiRenderUnit, focused: Option<&str>) -> bool {
    matches!(
        vm,
        TuiRenderUnit::TuiToolCard(t)
            if !t.is_running && !t.is_error && t.diff.is_none()
                && matches!(t.presentation, TuiToolPresentation::Generic)
                && !t.user_modified
                && focused != Some(t.tool_id.as_str())
    )
}

fn is_error_card(vm: &TuiRenderUnit) -> bool {
    matches!(vm, TuiRenderUnit::TuiToolCard(t) if t.is_error)
}

/// 原样输出区间内的稳定切点登记——区间按原样输出，故输出下标 =
/// `out_before + (c - span_start)`。只有**非可合并条目**能作切点：可合并条目的
/// 分组结果依赖后一条目（相邻性）。error 条目除外——可能是前一组失败 lookahead
/// 的一部分，故不登记（其收口处的切点由 [`group_successful_tools`] 单独登记）。
fn register_span_cuts(
    cuts: &mut Vec<GroupCut>,
    span: &im::Vector<TuiRenderUnit>,
    span_start: usize,
    out_before: usize,
    focused: Option<&str>,
) {
    for (k, vm) in span.iter().enumerate() {
        if !is_error_card(vm) && !is_mergeable(vm, focused) {
            cuts.push(GroupCut {
                input: span_start + k + 1,
                output: out_before + k + 1,
                anchor: CutAnchor::AfterCard,
            });
        }
    }
}

/// [§7] 相邻、成功、低信息密度 tools 压成 `TuiCollapsedGroup`。
///
/// - 可合并：已完成（!running）、非 error、无 diff（diff-edit 不隐藏）、
///   Generic presentation（Skill/Todo 语义卡片保留）、未被用户手动操作
///   （`user_modified`——折叠 pass 已把 FOLD_OVERRIDES 复写到该标志）、
///   非当前 selected entry（`FOCUSED_ENTRY` atom，消息区焦点导航写入）。
/// - 不可合并：running、error、interaction、含 diff 的 edit（当前无生产 diff，
///   由 `diff.is_some()` 守卫未来 Slice 5 路径）、当前 selected entry（焦点在
///   消息区侧，按身份键免疫——见 atoms.rs `FOCUSED_ENTRY` 注释）。
/// - 只扫描 `[start..]` 段（current_turn 部分），不跨 assistant 正文 /
///   system event / turn 边界。
/// - 标题按工具名聚合成 `Read 3 · Glob 2` 形式（隐藏数随 title 展示）。
///
/// [Why 位置] 必须放在快照组装（push_view_models）而非 sync_cache：分组会删除
/// cached_view_models 元素，破坏 segment↔cache 索引对齐；快照层是纯视觉变换。
///
/// [PERF] 增量重建（纯函数 memoization，命中即与全量重建等价，视觉行为不变）：
/// 1. 段与上次**逐条一致**（[`same_entry`]）→ 整段复用缓存输出，零重建；
/// 2. 仅尾部变化 → 取「首个差异下标 ≤ d 的最大稳定切点」（[`stable_cut`]），
///    复用其前缀（与缓存共享节点），只重建变化后缀——成本 ∝ 变化量，不随历史
///    长度增长；
/// 3. 辅助输入（焦点键 / Group 覆盖折叠态 / 语言版本，见 [`group_aux_fingerprint`]）
///    变化或缓存缺失 → 全量重建（无法定位影响范围）。
///
/// 旧实现把整段折成一个指纹、任何变化都全量重建，并在整段上逆序 `remove` +
/// `insert` 就地改写：im::Vector 的节点与快照共享，每次 remove/insert 都触发
/// COW 路径复制（整 chunk 渲染单元深拷贝），长会话下每事件成本随历史线性增长。
fn group_successful_tools(items: &mut im::Vector<TuiRenderUnit>, start: usize) {
    // 拆分 current_turn 段——快照层是纯视觉变换，不触碰 segment↔cache 索引对齐。
    // [PERF] `start == 0`（生产唯一取值）等价于取走整个向量：`std::mem::take`
    // 是 O(1)，而 `split_off(0)` 实测有 ~17 µs 的常数开销（`Focus` 窗口重建）。
    let segment = if start == 0 {
        std::mem::take(&mut *items)
    } else {
        items.split_off(start)
    };

    // [§7 免疫] 焦点所在 entry 的身份键——焦点工具不得被并入折叠组
    // （用户正与之交互；入组后焦点 index 落到组上、展开态丢失）。
    // [S2 单一事实源] 读 FOCUSED_ENTRY 的派生 key（guard 绑定变量——focused
    // 借用 guard，生命周期须覆盖使用侧）；该读值同时供辅助指纹使用。
    let focus_state = crate::kit::atoms::FOCUSED_ENTRY.state();
    let focused_entry = focus_state.read();
    let focused = focused_entry
        .as_ref()
        .and_then(|f| f.key.as_ref())
        .and_then(|k| match k {
            FoldKey::Tool(id) => Some(id.as_str()),
            _ => None,
        });
    let aux = group_aux_fingerprint(focused);

    // ── 复用判定：首个差异下标 → ≤ 差异点的最大稳定切点 ──
    let cache = TOOL_GROUP_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let (rebuild_from, mut grouped, mut cuts) = match cache.as_ref() {
        Some(c) if c.aux == aux => {
            let d = first_divergence(&c.input, &segment);
            if d == segment.len() && d == c.input.len() {
                // 段与上次逐条一致（无变化的重复发布）——整段复用。
                (d, c.grouped.clone(), c.cuts.clone())
            } else {
                match stable_cut(&c.cuts, &segment, d, focused) {
                    Some((in_cut, out_cut)) => {
                        // 复用前缀与缓存共享节点（O(log n) 切片，元素零克隆）。
                        let mut prefix = c.grouped.clone();
                        prefix.split_off(out_cut);
                        let kept = c
                            .cuts
                            .iter()
                            .copied()
                            .take_while(|cut| cut.input <= in_cut)
                            .collect();
                        (in_cut, prefix, kept)
                    }
                    None => (0, im::Vector::new(), Vec::new()),
                }
            }
        }
        _ => (0, im::Vector::new(), Vec::new()),
    };
    if rebuild_from == segment.len() {
        #[cfg(test)]
        crate::kit::acp_bridge::observe_perf(
            crate::kit::acp_bridge::PerfCounter::GroupFullReuse,
            1,
        );
        drop(cache);
        join_into(items, grouped);
        return;
    }
    #[cfg(test)]
    {
        crate::kit::acp_bridge::observe_perf(crate::kit::acp_bridge::PerfCounter::GroupRebuild, 1);
        crate::kit::acp_bridge::observe_perf(
            crate::kit::acp_bridge::PerfCounter::GroupRebuiltUnits,
            (segment.len() - rebuild_from) as u64,
        );
    }
    drop(cache);

    // ── 单趟重建 [rebuild_from..)：原样区间与合并组交替输出 ──
    // 旧实现在整段上逆序 `remove` + `insert` 就地改写：im::Vector 的每个节点都是
    // Arc 共享节点（与 state.committed / 快照共享），任何 remove/insert 都触发
    // COW 路径复制（整 chunk 元素深拷贝）——成本 O(段长)、且每次事件全量重来。
    // 新实现只做 `append`/`push_back`（不改写共享节点），deep clone 仅限真正进入
    // 折叠组的卡片。
    // 完整输入留作下次比较基准（O(1) clone；im 为持久化结构——对 `rest` 的
    // split_off 不会改动共享节点，`input` 始终是完整段）。
    let input = segment.clone();
    let mut rest = segment;
    if rebuild_from > 0 {
        rest = rest.split_off(rebuild_from);
    }
    let mut base = rebuild_from;
    #[cfg(test)]
    let mut copied_units: u64 = 0;
    let mut i = 0usize;
    while i < rest.len() {
        if !is_mergeable(rest.get(i).expect("i < rest.len()"), focused) {
            i += 1;
            continue;
        }
        let run_start = i;
        while i < rest.len() && is_mergeable(rest.get(i).expect("i < rest.len()"), focused) {
            i += 1;
        }
        // 单张可合并卡不组——留在原样区间内（其分组结果依赖相邻条目）。
        let run_len = i - run_start;
        if run_len < 2 {
            continue;
        }
        // 原样区间 [0, run_start)：整体 append（与源树共享节点，元素零克隆）。
        if run_start > 0 {
            let tail = rest.split_off(run_start);
            let span = std::mem::replace(&mut rest, tail);
            register_span_cuts(&mut cuts, &span, base, grouped.len(), focused);
            base += span.len();
            join_into(&mut grouped, span);
        }
        // 该 run → 一张折叠组；组内容（隐藏卡片）是本函数唯一的深拷贝来源。
        let after = rest.split_off(run_len);
        let run = std::mem::replace(&mut rest, after);
        let failed_count = rest.iter().take_while(|vm| is_error_card(vm)).count() as u32;
        #[cfg(test)]
        {
            copied_units += run_len as u64;
        }
        grouped.push_back(build_collapsed_group(&run, failed_count));
        // 组收口处的稳定切点（见 [`CutAnchor::AfterGroup`]）——长会话最常见的
        // 切点：末组之后新增内容时，前缀（含本组）可直接复用。
        cuts.push(GroupCut {
            input: base + run_len,
            output: grouped.len(),
            anchor: CutAnchor::AfterGroup,
        });
        // 失败连串收口处的稳定切点：前缀含完整 error 连串，组的 failed_count
        // 已定（见 [`CutAnchor::AfterCard`]）。
        let err_run = rest.iter().take_while(|vm| is_error_card(vm)).count();
        if err_run > 0 {
            cuts.push(GroupCut {
                input: base + run_len + err_run,
                output: grouped.len() + err_run,
                anchor: CutAnchor::AfterCard,
            });
        }
        base += run_len;
        i = 0;
    }
    if !rest.is_empty() {
        register_span_cuts(&mut cuts, &rest, base, grouped.len(), focused);
        base += rest.len();
        join_into(&mut grouped, rest);
    }
    // 段尾切点：整段（或 [rebuild_from..] 末尾）总是可复用的复选点——按新输入
    // 重判，尾条目可合并/可续接 error 时由 [`cut_stable`] 拒绝。
    cuts.push(GroupCut {
        input: base,
        output: grouped.len(),
        anchor: CutAnchor::AfterCard,
    });
    cuts.dedup_by_key(|cut| cut.input);
    if cuts.len() > GROUP_CUT_CAP {
        cuts.drain(0..cuts.len() - GROUP_CUT_CAP);
    }

    #[cfg(test)]
    crate::kit::acp_bridge::observe_perf(
        crate::kit::acp_bridge::PerfCounter::GroupCopiedUnits,
        copied_units,
    );

    // [PERF] 缓存本次分组结果（clone = O(1) 引用计数共享）。
    *TOOL_GROUP_CACHE.lock().unwrap_or_else(|e| e.into_inner()) = Some(ToolGroupCache {
        input,
        grouped: grouped.clone(),
        cuts,
        aux,
    });
    join_into(items, grouped);
}

/// 由一段相邻可合并工具卡构造折叠组（§7 标题聚合 `Read 3 · Glob 2`）。
fn build_collapsed_group(run: &im::Vector<TuiRenderUnit>, failed_count: u32) -> TuiRenderUnit {
    use crate::kit::tui_render_unit::TuiCollapsedGroup;

    let mut names: Vec<(String, u32)> = Vec::new();
    let mut hidden_vms: Vec<TuiRenderUnit> = Vec::with_capacity(run.len());
    for vm in run.iter() {
        if let TuiRenderUnit::TuiToolCard(t) = vm {
            let display = crate::kit::tool_display::format_tool_name(&t.tool_name);
            match names.iter_mut().find(|(n, _)| *n == display) {
                Some((_, c)) => *c += 1,
                None => names.push((display, 1)),
            }
        }
        hidden_vms.push(vm.clone());
    }
    let title = names
        .into_iter()
        .map(|(name, count)| format!("{name} {count}"))
        .collect::<Vec<_>>()
        .join(" \u{b7} ");
    let group_key = FoldKey::Group(
        hidden_vms
            .iter()
            .filter_map(|vm| match vm {
                TuiRenderUnit::TuiToolCard(t) => Some(t.tool_id.clone()),
                _ => None,
            })
            .collect(),
    );
    let fold = FOLD_OVERRIDES
        .state()
        .read()
        .get(&group_key)
        .copied()
        .unwrap_or(FoldState::Collapsed);
    let mut group = TuiCollapsedGroup {
        title,
        count: run.len() as u32,
        failed_count,
        view_models: hidden_vms,
        fold,
        content_hash: 0,
    };
    group.recompute_hash();
    TuiRenderUnit::TuiCollapsedGroup(group)
}

/// [G2] 折叠状态机单点 pass——spec §7 折叠表 + FOLD_OVERRIDES 用户覆盖。
///
/// 对每个带 fold 字段的 VM 计算目标 fold，与现值不同才 COW set + 重算 hash（G1）：
/// - 表值来自 [`fold_for_status`]（tui_render_unit.rs 唯一策略单点）；
/// - FOLD_OVERRIDES 中的 key 永远优先——用户手动操作，自动策略免疫
///   （spec §7「running 变 completed 时，仅未被手动操作的 entry 可自动折叠」）；
/// - 带覆盖的 VM 同时恢复 `user_modified=true`（流式重建后免疫仍成立）；
/// - reasoning 状态推导：trailing 流式段（build_bubble_parts running=true）
///   为 Running；phase 离开 PromptRunning → 全部 Completed。
///
/// [G3] 逐 token 调用，但只对变化项做克隆+set：稳态下（无流式、无覆盖变更）
/// 是 O(N) 只读扫描，零写入。
fn apply_fold_pass(items: &mut im::Vector<TuiRenderUnit>, phase: SessionPhase) {
    use TuiRenderUnit::*;
    // [PERF] 读引用代替快照克隆——表只被键盘 handler 低频写入，pass 本身
    // 不写该表（迭代期只读 + 末尾 COW set，无嵌套锁获取），持读锁安全；
    // 空表短路：热路径（无手动覆盖）跳过全部查表与 FoldKey 构造克隆。
    let overrides_state = FOLD_OVERRIDES.state();
    let overrides = overrides_state.read();
    let has_overrides = !overrides.is_empty();
    let mut updates: Vec<(usize, TuiRenderUnit)> = Vec::new();

    for (i, vm) in items.iter().enumerate() {
        match vm {
            TuiAssistantBubble(b) => {
                // ① reasoning 状态推导：phase 离开 PromptRunning → 全部 Completed。
                // ② 正文时长冻结（§6.2 `12.4s`）：phase 离开 PromptRunning 时，
                //    持有 started_at 的 bubble（trailing 流式段——冻结段在
                //    build_bubble_parts 中恒 None）冻结 duration_ms，镜像
                //    reasoning 的冻结机制。快照在 TurnDone 后静态，冻结值持续。
                //
                // [PERF §15] 先对借用 `b` 做只读判定，命中变化才 clone——
                // 稳态下（无流式、无覆盖变更）零克隆零写入。
                let mut changed = false;
                // reasoning 翻转参数：(fold, status, is_running, 冻结时长 ms)
                let mut reasoning_update: Option<(FoldState, EntryStatus, bool, Option<u64>)> =
                    None;
                if let Some(r) = b.reasoning.as_ref() {
                    // 状态推导：phase 离开 PromptRunning → 全部 Completed。
                    let mut status = r.status;
                    if phase != SessionPhase::PromptRunning && status == EntryStatus::Running {
                        status = EntryStatus::Completed;
                    }
                    // 用户手动展开（覆盖表中存在 Reasoning(message_id)）→ 覆盖优先。
                    // 空表短路：无手动覆盖时不构造 FoldKey（避免逐 token 克隆
                    // message_id）。
                    let override_fold = if has_overrides {
                        b.message_id
                            .as_ref()
                            .and_then(|id| overrides.get(&FoldKey::Reasoning(id.clone())).copied())
                    } else {
                        None
                    };
                    let target_fold = override_fold
                        .unwrap_or_else(|| fold_for_status(FoldTarget::Reasoning, status));
                    let fold_changed = r.fold != target_fold;
                    let status_changed =
                        r.status != status || r.is_running != (status == EntryStatus::Running);
                    if fold_changed || status_changed {
                        // Running → Completed 时冻结时长（§6.3 `Thought for 12s`）：
                        // started_at 只属于流式段，冻结后置 None，时长不再增长。
                        let frozen =
                            (status == EntryStatus::Completed && r.is_running).then(|| {
                                r.started_at
                                    .map(|t| t.elapsed().as_millis() as u64)
                                    .unwrap_or(0)
                            });
                        reasoning_update =
                            Some((target_fold, status, status == EntryStatus::Running, frozen));
                        changed = true;
                    }
                }
                // 正文时长冻结（§6.2）：仅 trailing 流式段持有 started_at。
                let text_freeze = phase != SessionPhase::PromptRunning && b.started_at.is_some();
                if changed || text_freeze {
                    let mut updated = b.clone();
                    if let Some((fold, status, is_running, frozen)) = reasoning_update {
                        let r = updated.reasoning.as_mut().expect("reasoning_update 必有块");
                        r.fold = fold;
                        if let Some(ms) = frozen {
                            r.duration_ms = Some(ms);
                            r.started_at = None;
                        }
                        r.status = status;
                        r.is_running = is_running;
                    }
                    if text_freeze {
                        updated.duration_ms = Some(
                            b.started_at
                                .map(|t| t.elapsed().as_millis() as u64)
                                .unwrap_or(0),
                        );
                        updated.started_at = None;
                    }
                    updated.recompute_hash();
                    updates.push((i, TuiAssistantBubble(updated)));
                }
            }
            TuiToolCard(t) => {
                let status = if t.is_running {
                    EntryStatus::Running
                } else if t.is_error {
                    EntryStatus::Error
                } else {
                    EntryStatus::Completed
                };
                let override_fold = if has_overrides {
                    overrides.get(&FoldKey::Tool(t.tool_id.clone())).copied()
                } else {
                    None
                };
                let user_modified = override_fold.is_some() || t.user_modified;
                let target_fold =
                    override_fold.unwrap_or_else(|| fold_for_status(FoldTarget::Tool, status));
                if t.fold != target_fold || t.user_modified != user_modified {
                    let mut updated = t.clone();
                    updated.fold = target_fold;
                    updated.user_modified = user_modified;
                    updated.recompute_hash();
                    updates.push((i, TuiToolCard(updated)));
                }
            }
            TuiSubAgentGroup(g) => {
                // parent 终态由 canonical is_error 决定（nested child tool
                // error 不提升 block error）；Error → §7 表 (SubAgent, Error)
                // => Expanded（与 tool error 展开语义一致）。
                let status = if g.is_running {
                    EntryStatus::Running
                } else if g.is_error {
                    EntryStatus::Error
                } else {
                    EntryStatus::Completed
                };
                let override_fold = if has_overrides {
                    overrides
                        .get(&FoldKey::SubAgent(g.instance_id.clone()))
                        .copied()
                } else {
                    None
                };
                let user_modified = override_fold.is_some() || g.user_modified;
                let target_fold =
                    override_fold.unwrap_or_else(|| fold_for_status(FoldTarget::SubAgent, status));
                if g.fold != target_fold || g.user_modified != user_modified {
                    let mut updated = g.clone();
                    updated.fold = target_fold;
                    updated.user_modified = user_modified;
                    updated.recompute_hash();
                    updates.push((i, TuiSubAgentGroup(updated)));
                }
            }
            TuiSystemReminder(r) => {
                let target_fold = if has_overrides {
                    overrides
                        .get(&FoldKey::SystemReminder(r.reminder_id))
                        .copied()
                        .unwrap_or_else(|| {
                            fold_for_status(FoldTarget::System, EntryStatus::Completed)
                        })
                } else {
                    fold_for_status(FoldTarget::System, EntryStatus::Completed)
                };
                if r.fold != target_fold {
                    let mut updated = r.clone();
                    updated.fold = target_fold;
                    updated.recompute_hash();
                    updates.push((i, TuiSystemReminder(updated)));
                }
            }
            TuiAskUserBlock(a) => {
                // [Slice 4 §6.8] 状态推导：pending → Running（Expanded 可聚焦，
                // 等待期间锚定）；结果回写（pending=false）→ Completed；error
                // 优先。折叠策略来自 fold_for_status 的 Interaction 行
                // （Running→Expanded / Completed→Collapsed / Error→Expanded）。
                let status = if a.is_error {
                    EntryStatus::Error
                } else if a.pending {
                    EntryStatus::Running
                } else {
                    EntryStatus::Completed
                };
                // 用户手动展开过（覆盖表存在 Interaction(request_id)）→ 覆盖优先
                let override_fold = if has_overrides {
                    a.request_id
                        .as_ref()
                        .and_then(|id| overrides.get(&FoldKey::Interaction(id.clone())).copied())
                } else {
                    None
                };
                let user_modified = override_fold.is_some() || a.user_modified;
                let target_fold = override_fold
                    .unwrap_or_else(|| fold_for_status(FoldTarget::Interaction, status));
                if a.fold != target_fold || a.user_modified != user_modified {
                    let mut updated = a.clone();
                    updated.fold = target_fold;
                    updated.user_modified = user_modified;
                    updated.recompute_hash();
                    updates.push((i, TuiAskUserBlock(updated)));
                }
            }
            _ => {}
        }
    }

    #[cfg(test)]
    crate::kit::acp_bridge::observe_perf(
        crate::kit::acp_bridge::PerfCounter::FoldPassWrites,
        updates.len() as u64,
    );

    for (i, vm) in updates {
        items.set(i, vm);
    }
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
    FOLD_OVERRIDES.state().write().clear();
    // [S2 §3.4] 焦点单一事实源同源清空（跨 session 身份不唯一——slot 与 key
    // 都依赖旧会话索引/身份，残留会让新会话焦点/免疫错误指向）。
    *crate::kit::atoms::FOCUSED_ENTRY.state().write() = None;
    let snapshot = ViewModelsSnapshot {
        items: im::Vector::new(),
        generation: 0,
    };
    *VIEW_MODELS.state().write() = snapshot;
}

/// 将 BridgeState 中的状态快照写入 ACP_STATE Atom。
///
/// 仅在快照值变化时才写入——避免不必要的全树重渲染。
/// 流式期间 variant/is_loading 不变时，仅 view_count 变化；
/// popup 状态由各自的独立 atom 追踪（SLASH_HINT_ACTIVE 等），
/// 不应写入 ACP_STATE 导致 AppShell 重渲染。
pub(crate) fn push_acp_state(state: &mut BridgeState) {
    let snapshot = AcpStateSnapshot {
        variant: state.variant,
        view_count: state.committed.len() + state.current_turn.view_models().len(),
        is_loading: state.phase == SessionPhase::PromptRunning,
        wizard_active: false,
        at_mention_active: *AT_MENTION_ACTIVE.state().read(),
        slash_hint_active: *SLASH_HINT_ACTIVE.state().read(),
    };
    let state_ref = ACP_STATE.state();
    let mut acp = state_ref.write();
    if *acp != snapshot {
        *acp = snapshot;
    }
}

/// 将 BridgeState.popup_kind 写入 POPUP_KIND Atom（S7）。
pub(crate) fn push_popup_kind(state: &BridgeState) {
    *POPUP_KIND.state().write() = state.popup_kind;
}

/// 将 `INPUT_BUFFER` atom 中所有排队输入按入队顺序 drain，逐条发送到 SUBMIT_TX。
///
/// 调用时机：`TurnDone` 事件与取消复位（stale / 非 stale）——agent 结束本轮或
/// 复位，从队列里取出用户在 loading 期间缓存的 agent text 继续提交。若 buffer
/// 为空则 no-op；若 SUBMIT_TX 未初始化也安全跳过。
///
/// [Slice 3 D4] §10 queued 反转：排队项在 loading 期间**不进 transcript**（只
/// 显示在 composer 上方队列），drain 时镜像非 loading 提交路径——先
/// `send_local_user_bubble(text)`（本地气泡恰出现一次，不依赖服务端回显）
/// 再 `tx.send(AgentText)`；`handle_local_user_bubble` 的 last_submitted_text /
/// turn_generation 语义与非 loading 提交完全一致。
///
/// 多条输入的顺序保证：VecDeque + 顺序 `tx.send` + submit_consumer 单消费者 →
/// 严格 FIFO。第一条立即触发 prompt，后续在 submit_consumer 内部顺序处理
/// （每条都等上一条的 RPC 完成）。
pub(crate) fn drain_input_buffer() {
    if crate::kit::steer_state::is_enabled() {
        return;
    }
    let tx = SUBMIT_TX.get().cloned();
    if tx.is_none() {
        return;
    }

    let drained: Vec<String> = INPUT_BUFFER.state().write().drain(..).collect();
    if let Some(tx) = tx {
        for text in drained {
            // [Slice 3 D4] 本地气泡 + 提交（镜像非 loading 路径）。
            crate::kit::input_area::send_local_user_bubble(&text);
            let _ = tx.send(SubmitRequest::AgentText(text));
        }
    }
}

/// 从 ACP SessionUpdate::Plan JSON 中提取 TodoItem 列表并写入 TODO_ITEMS atom。
///
/// 使用类型安全 serde 反序列化将 Plan JSON 映射为 TodoItem 列表。
/// Plan JSON 格式:
///   {"sessionUpdate":"plan","entries":[{"content":"Fix bug","status":"in_progress","priority":"medium"}]}
pub fn handle_plan_update(update: &serde_json::Value) {
    use crate::kit::message_area::{TodoItem, TodoStatus};
    use agent_client_protocol::schema::v1::{Plan, PlanEntryStatus};

    let plan: Plan = match serde_json::from_value(update.clone()) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = %e, "handle_plan_update: failed to deserialize Plan");
            return;
        }
    };

    tracing::debug!(
        entries_count = plan.entries.len(),
        "handle_plan_update: received Plan entries"
    );

    let items: Vec<TodoItem> = plan
        .entries
        .into_iter()
        .map(|e| {
            let status = match e.status {
                PlanEntryStatus::Pending => TodoStatus::Pending,
                PlanEntryStatus::InProgress => TodoStatus::InProgress,
                PlanEntryStatus::Completed => TodoStatus::Completed,
                _ => {
                    tracing::warn!(status = ?e.status, "handle_plan_update: unknown PlanEntryStatus, fallback to Pending");
                    TodoStatus::Pending
                }
            };
            TodoItem {
                content: e.content,
                status,
            }
        })
        .collect();

    tracing::debug!(
        "handle_plan_update: writing {} items to TODO_ITEMS",
        items.len()
    );
    *crate::kit::atoms::TODO_ITEMS.state().write() = items;
}
