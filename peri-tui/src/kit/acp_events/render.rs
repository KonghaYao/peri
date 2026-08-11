//! Render helper functions — push_view_models, push_acp_state, etc.

use super::*;
use crate::i18n;
use crate::kit::atoms::FOLD_OVERRIDES;
use crate::kit::submit_request::SubmitRequest;
use crate::kit::tui_render_unit::{
    EntryStatus, FoldKey, FoldState, FoldTarget, TuiDivider, TuiRenderUnit, TuiTodoSummary,
    TuiToolPresentation, fold_for_status, tui_hash_combine, tui_hash_str,
};
use fluent_bundle::FluentValue;
use std::sync::Mutex;

/// 将 BridgeState 中的 ViewModels 写入 VIEW_MODELS Atom。
///
/// 从 `state.committed`（im::Vector）clone（O(1)引用计数）后 `append`
/// `current_turn.view_models()`（O(log n)，与 current_turn 增量缓存共享元素，
/// 不再逐条深拷贝），构成扁平单层列表。generation 每次调用递增+1。
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
    let current_turn_start = items.len();
    items.append(state.current_turn.view_models().clone());

    // [G2] 折叠状态机单点 pass（spec §7 表 + FOLD_OVERRIDES 用户覆盖）。
    // [共享安全] items 通过 append 与 current_turn 缓存共享元素——pass 先收集
    // 翻转目标，再用 im::Vector::set（内部 COW）应用，避免就地修改共享节点。
    apply_fold_pass(&mut items, state.phase);

    // [§6.9] todo 进度摘要：活动 turn（current_turn 非空）且 TODO_ITEMS 非空时，
    // 插在 trailing 最终回答之前（回答后无 todo）；无 trailing 回答时位于 turn 底部。
    insert_todo_summary(&mut items, state.phase);

    // [§7] 相邻成功工具分组——只作用于 current_turn 段（不跨 turn 边界）。
    group_successful_tools(&mut items, current_turn_start);

    state.generation = state.generation.wrapping_add(1);
    let snapshot = ViewModelsSnapshot {
        items,
        generation: state.generation,
    };
    tracing::trace!(target: "frozen_diag", gen = state.generation, "bridge: acquiring VIEW_MODELS write lock");
    *VIEW_MODELS.state().write() = snapshot;
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
/// 指纹覆盖全部影响分组结果的输入（段内条目、焦点键）；命中时复用上次分组
/// 结果，普通文本 token 跳过全段深克隆重建（深克隆 = 每条目 String 复制 +
/// format_tool_name 聚合 + items remove/insert 重排）。
struct ToolGroupCache {
    /// 输入指纹（见 [`group_input_fingerprint`]）。
    fingerprint: u64,
    /// 上次分组后的段结果——im::Vector clone 为 O(1) 引用计数共享，命中时
    /// 零复制替换回 items。
    grouped: im::Vector<TuiRenderUnit>,
    /// 上次输入段末尾是否为 assistant bubble（流式 bubble 恒为段末元素——
    /// sync_cache 布局保证：index = segments.len()；分组不移动它，命中时用
    /// 当前末元素原位替换缓存副本，故流式内容不进指纹）。
    has_trailing_bubble: bool,
}

/// 流式 bubble 的指纹身份标记（其内容不参与分组决策）。
const TRAILING_BUBBLE_MARKER: u64 = 0x5EAB_1E5E;
/// TuiToolCard 的指纹变体码。
const TOOL_VARIANT_CODE: u64 = 3;

static TOOL_GROUP_CACHE: Mutex<Option<ToolGroupCache>> = Mutex::new(None);

/// 计算影响分组结果的输入指纹——只读扫描，无克隆无分配。
///
/// 逐条目组合（变体码 + content_hash；trailing 流式 bubble 只计身份标记），
/// 末尾追加段长与焦点键。段长保证空段与任何非空段指纹必然区分；变体码 +
/// 工具卡显式字段兜底手造条目 content_hash=0 的测试场景（生产路径
/// content_hash 已覆盖全部显示字段，是渲染缓存的事实键）。
fn group_input_fingerprint(segment: &im::Vector<TuiRenderUnit>) -> (u64, bool) {
    use std::hash::{Hash, Hasher};
    let mut h: u64 = 0;
    let last = segment.len().saturating_sub(1);
    for (i, vm) in segment.iter().enumerate() {
        let is_trailing_bubble = i == last && matches!(vm, TuiRenderUnit::TuiAssistantBubble(_));
        h = if is_trailing_bubble {
            tui_hash_combine(h, TRAILING_BUBBLE_MARKER)
        } else {
            match vm {
                TuiRenderUnit::TuiToolCard(t) => {
                    // 合并决策相关字段显式纳入（tool_id 参与焦点免疫比较、
                    // tool_name 参与标题聚合、flags 决定可合并性）。
                    let mut eh = tui_hash_combine(TOOL_VARIANT_CODE, vm.content_hash());
                    eh = tui_hash_combine(eh, tui_hash_str(&t.tool_id));
                    eh = tui_hash_combine(eh, tui_hash_str(&t.tool_name));
                    let flags = u64::from(t.is_running)
                        | (u64::from(t.is_error) << 1)
                        | (u64::from(t.diff.is_some()) << 2)
                        | (u64::from(t.user_modified) << 3)
                        | (u64::from(matches!(t.presentation, TuiToolPresentation::Generic)) << 4);
                    tui_hash_combine(eh, flags)
                }
                other => {
                    let code = match other {
                        TuiRenderUnit::TuiUserBubble(_) => 1,
                        TuiRenderUnit::TuiAssistantBubble(_) => 2,
                        TuiRenderUnit::TuiSystemNote(_) => 4,
                        TuiRenderUnit::TuiSubAgentGroup(_) => 5,
                        TuiRenderUnit::TuiCollapsedGroup(_) => 6,
                        TuiRenderUnit::TuiDivider(_) => 7,
                        TuiRenderUnit::TuiAskUserBlock(_) => 8,
                        TuiRenderUnit::TuiTodoSummary(_) => 9,
                        TuiRenderUnit::TuiToolCard(_) => unreachable!(),
                    };
                    tui_hash_combine(code, other.content_hash())
                }
            }
        };
    }
    h = tui_hash_combine(h, segment.len() as u64);
    // [S2 单一事实源] 焦点键——焦点工具免疫随焦点变化切换（命中/未命中必须
    // 一致覆盖）。只 hash `f.key`（Option<FoldKey>），不 hash 整个 FocusedEntry
    // ——含 slot 会使 key=None 的焦点移动（无折叠能力 entry 间导航）无谓失效
    // TOOL_GROUP_CACHE 指纹。Option<&FoldKey> 与 Option<FoldKey> 的 Hash
    // 逐位一致（引用 Hash 转发到底层值）。
    let mut fh = std::collections::hash_map::DefaultHasher::new();
    let focus_state = crate::kit::atoms::FOCUSED_ENTRY.state();
    focus_state
        .read()
        .as_ref()
        .and_then(|f| f.key.as_ref())
        .hash(&mut fh);
    h = tui_hash_combine(h, fh.finish());
    let has_trailing_bubble = matches!(segment.back(), Some(TuiRenderUnit::TuiAssistantBubble(_)));
    (h, has_trailing_bubble)
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
/// [PERF] 快速路径：输入指纹与上次一致（工具卡无新增/状态/焦点/覆盖变化——
/// 覆盖已由折叠 pass 复写到 user_modified，随卡片 hash 纳入指纹）时复用上次
/// 分组结果，普通文本 token 跳过全段深克隆重建。命中即与重建等价（纯函数
/// memoization），视觉行为不变。
fn group_successful_tools(items: &mut im::Vector<TuiRenderUnit>, start: usize) {
    use crate::kit::tui_render_unit::TuiCollapsedGroup;

    // 拆分 current_turn 段（O(log n)），快速路径在段上判定。
    let mut segment = items.split_off(start);
    let (fingerprint, has_trailing_bubble) = group_input_fingerprint(&segment);
    let cache = TOOL_GROUP_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(c) = cache.as_ref()
        && c.fingerprint == fingerprint
        && c.has_trailing_bubble == has_trailing_bubble
    {
        let mut grouped = c.grouped.clone(); // O(1) 引用计数共享，零深拷贝
        if has_trailing_bubble && !grouped.is_empty() {
            // 流式 bubble 内容不进指纹：弹出现段末元素，原位替换缓存段末的
            // 旧 bubble（分组从不移动 bubble——它是段末非工具条目，runs 到
            // 它为止）。
            if let Some(current_last) = segment.pop_back() {
                grouped.set(grouped.len() - 1, current_last);
            }
        }
        items.append(grouped);
        return;
    }
    drop(cache);

    // [§7 免疫] 焦点所在 entry 的身份键——焦点工具不得被并入折叠组
    // （用户正与之交互；入组后焦点 index 落到组上、展开态丢失）。
    // 读引用代替 clone；按需取 Tool 变体的 id 字符串比较，免去逐卡克隆
    // tool_id 构造 FoldKey。[S2 单一事实源] 读 FOCUSED_ENTRY 的派生 key
    //（guard 绑定变量——focused 借用 guard，生命周期须覆盖使用处）。
    let focus_state = crate::kit::atoms::FOCUSED_ENTRY.state();
    let focused_entry = focus_state.read();
    let focused = focused_entry
        .as_ref()
        .and_then(|f| f.key.as_ref())
        .and_then(|k| match k {
            FoldKey::Tool(id) => Some(id.as_str()),
            _ => None,
        });

    // 扫描 segment 的相邻可合并工具段（split_off 后为相对索引，等价原 [start..]）。
    let mut runs: Vec<(usize, usize)> = Vec::new(); // (run_start, run_end_exclusive)
    let mut run_start: Option<usize> = None;
    for i in 0..segment.len() {
        let mergeable = matches!(
            segment.get(i),
            Some(TuiRenderUnit::TuiToolCard(t))
                if !t.is_running && !t.is_error && t.diff.is_none()
                    && matches!(t.presentation, TuiToolPresentation::Generic)
                    // §7「用户手动改变 fold state 后不再被自动策略覆盖」：
                    // 手动展开/折叠过的工具保持独立（覆盖键随折叠 pass 复写
                    // 到 user_modified，此处一并防 FOLD_OVERRIDES 残留）。
                    && !t.user_modified
                    && focused != Some(t.tool_id.as_str())
        );
        match (mergeable, run_start) {
            (true, None) => run_start = Some(i),
            (true, Some(_)) => {}
            (false, Some(s)) => {
                runs.push((s, i));
                run_start = None;
            }
            (false, None) => {}
        }
    }
    if let Some(s) = run_start {
        runs.push((s, segment.len()));
    }

    // 逆序应用（删除后面的元素不影响前面的索引）。
    for (run_start, run_end) in runs.into_iter().rev() {
        let run_len = run_end - run_start;
        if run_len < 2 {
            continue;
        }
        // [D2] 失败数 = 从 run 结束位置向后扫描**连续相邻** error 工具计数。
        // error 工具不入组、不删除、保持展开（§7 表 error→Expanded + §15
        // 「error 永不隐藏」优先）；扫描在删除 run 元素之前进行（segment 索引
        // 仍指向原位置）。
        let mut failed_count: u32 = 0;
        for i in run_end..segment.len() {
            let is_error = matches!(
                segment.get(i),
                Some(TuiRenderUnit::TuiToolCard(t)) if t.is_error
            );
            if is_error {
                failed_count += 1;
            } else {
                break;
            }
        }
        // 按工具名聚合标题：`Read 3 · Glob 2`
        let mut names: Vec<(String, u32)> = Vec::new();
        let mut hidden_vms: Vec<TuiRenderUnit> = Vec::with_capacity(run_len);
        for i in run_start..run_end {
            if let Some(TuiRenderUnit::TuiToolCard(t)) = segment.get(i) {
                let display = crate::kit::tool_display::format_tool_name(&t.tool_name);
                match names.iter_mut().find(|(n, _)| *n == display) {
                    Some((_, c)) => *c += 1,
                    None => names.push((display, 1)),
                }
                hidden_vms.push(segment.get(i).cloned().unwrap());
            }
        }
        let title = names
            .into_iter()
            .map(|(name, count)| format!("{name} {count}"))
            .collect::<Vec<_>>()
            .join(" \u{b7} ");
        let mut group = TuiCollapsedGroup {
            title,
            count: run_len as u32,
            failed_count,
            view_models: hidden_vms,
            content_hash: 0,
        };
        group.recompute_hash();
        // 删除 run 内元素（逆序删，索引稳定），再把组放到 run_start。
        for i in (run_start..run_end).rev() {
            segment.remove(i);
        }
        segment.insert(run_start, TuiRenderUnit::TuiCollapsedGroup(group));
    }

    // [PERF] 缓存本次分组结果（clone = O(1) 引用计数共享）。
    *TOOL_GROUP_CACHE.lock().unwrap_or_else(|e| e.into_inner()) = Some(ToolGroupCache {
        fingerprint,
        grouped: segment.clone(),
        has_trailing_bubble,
    });
    items.append(segment);
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
                let status = if g.is_running {
                    EntryStatus::Running
                } else {
                    EntryStatus::Completed
                };
                let override_fold = if has_overrides {
                    overrides
                        .get(&FoldKey::SubAgent(g.agent_id.clone()))
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

    for (i, vm) in updates {
        items.set(i, vm);
    }
}

/// 由 acp_bridge 在 BRIDGE_RESET_COUNTER 复位时调用——
/// 立即将空快照写入 VIEW_MODELS atom，防止其他 reader 读到旧 session 数据。
pub fn push_view_models_for_reset() {
    // [Slice 2] session 复位时清空折叠覆盖表——tool_id/message_id/agent_id
    // 跨 session 不保证唯一，残留覆盖会错误作用于新会话的同名 entry。
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
