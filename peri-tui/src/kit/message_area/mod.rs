//! MessageArea：直接读取 VIEW_MODELS，通过 vm_to_lines 将 TuiRenderUnit
//! 转换为 Vec<Line>，按视口裁剪后渲染。
//!
//! - 滚动：由自持 ScrollPos（usize 偏移）处理键盘/鼠标事件（offset 管理）
//! - 渲染：视口裁剪——只 clone + highlight + 渲染视口内 ~60 行，避免 O(N×W) per render
//! - 智能跟随：use_effect 检测 VIEW_MODELS 变化
//! - 不再使用 RENDER_CACHE / render_bridge / ScrollView / wrap_map（已替换为 wrap_map_cache）

#![allow(clippy::needless_update)]

use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::kit::atoms::{
    BRIDGE_RESET_COUNTER, FOCUSED_ENTRY_KEY, FOLD_OVERRIDES, KEEPGOING_BLOCKED_UNTIL, LANG_VERSION,
    LOADING_EPOCH, RENDER_HEARTBEAT, SELECTED_SUBAGENT_ID, SUBMIT_TX, VIEW_MODELS,
    ViewModelsSnapshot,
};
use crate::kit::focus_router;
use crate::kit::mouse_router;
use crate::kit::submit_request::SubmitRequest;
use crate::kit::text_selection::TextSelection;
use crate::kit::tui_render_unit::{
    FoldKey, FoldState, InteractionKind, TuiAskUserBlock, TuiAssistantBubble, TuiRenderUnit,
};
use crate::kit::welcome::Welcome;
use peri_theme::atoms::{PALETTE_ATOM, THEME_ATOM};
use ratatui_kit::{
    crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind},
    prelude::*,
    ratatui::{
        layout::{Constraint, Direction},
        style::{Modifier, Style},
        text::{Line, Span, Text as RatText},
        widgets::{Block, Padding, Paragraph, Wrap},
    },
};

mod footer;
pub(crate) mod grid;
mod props;
pub(crate) mod render;
pub(crate) mod scroll;
mod selection;
pub(crate) use footer::hash_todo_items;
use footer::{KeepGoingLayout, build_footer_lines};
pub use footer::{TodoItem, TodoStatus};
pub use props::MessageAreaProps;
use props::{MsgAreaTracker, ScrollbarFields, ScrollbarHook};
use render::vm_to_lines_cached;
use scroll::{DragThrottle, ScrollThrottle, ScrollbarDragState};
use selection::{
    WrappedLineInfo, build_wrap_map, concat_wrap_maps, copy_to_clipboard,
    highlight_line_in_selection, mark_copy_message, viewport_logical_range, visual_to_logical,
};

/// keepgoing 按钮点击防抖时长（连续点击冷却）。
const KEEPGOING_DEBOUNCE: Duration = Duration::from_millis(1500);

/// 计算 palette 中影响 markdown 渲染的关键字段哈希。
/// 当主题切换时，hash 变化 → 触发 vm_caches 重建 → markdown 色值更新。
fn palette_markdown_key(p: &ratatui_kit::prelude::Palette) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    p.fg.hash(&mut h);
    p.bg.hash(&mut h);
    p.fg_dim.hash(&mut h);
    p.accent.hash(&mut h);
    p.surface.hash(&mut h);
    p.border.hash(&mut h);
    p.success.hash(&mut h);
    p.warning.hash(&mut h);
    p.error.hash(&mut h);
    p.info.hash(&mut h);
    h.finish()
}

/// NO_COLOR 剥离 pass（§12）：剥离可见行的前景/背景/下划线色，**保留 modifier**
/// （bold/italic/dim 等）与符号、文本——任何状态都不能只依赖颜色。
///
/// [G3] 只作用于视口裁剪后的可见行（视口行数 ≈ 终端高度），不触碰渲染缓存，
/// 不写业务状态；颜色剥离后的行仅本帧使用。
fn strip_line_colors(
    line: &ratatui_kit::ratatui::text::Line<'static>,
) -> ratatui_kit::ratatui::text::Line<'static> {
    // Line 级 style 与 span 级 style 同等处理：剥离颜色、保留 modifier。
    let line_style = strip_style_color(line.style);
    let spans = line
        .spans
        .iter()
        .map(|span| {
            ratatui_kit::ratatui::text::Span::styled(
                span.content.clone(),
                strip_style_color(span.style),
            )
        })
        .collect();
    ratatui_kit::ratatui::text::Line {
        spans,
        alignment: line.alignment,
        style: line_style,
    }
}

/// 剥离单个 Style 的颜色字段（fg/bg/underline_color），保留 modifier。
fn strip_style_color(s: ratatui_kit::ratatui::style::Style) -> ratatui_kit::ratatui::style::Style {
    ratatui_kit::ratatui::style::Style {
        fg: None,
        bg: None,
        underline_color: None,
        add_modifier: s.add_modifier,
        sub_modifier: s.sub_modifier,
    }
}

// ── entry 焦点导航纯函数（Slice 2 键盘语义层，视觉归 Slice 3）──────────────

/// 移动 entry 焦点：`Alt+Up`（delta=-1）/ `Alt+Down`（delta=+1）。
/// 无焦点时向上 → 最新 entry（末项），向下 → 首 entry；到达边界后钳制（不循环）。
fn move_entry_focus(items_len: usize, current: Option<usize>, delta: i32) -> Option<usize> {
    if items_len == 0 {
        return None;
    }
    let base: i64 = match current {
        Some(i) => i as i64,
        None if delta < 0 => items_len as i64, // 从无焦点向上 → 最新 entry
        None => -1,                            // 从无焦点向下 → 首 entry 之前
    };
    let next = base + i64::from(delta);
    Some(next.clamp(0, items_len as i64 - 1) as usize)
}

/// entry 的折叠键 + 当前 fold（无折叠能力的 entry → `None`）。
/// 与折叠 pass（`acp_events/render.rs::apply_fold_pass`）的键控口径一致：
/// Reasoning(message_id) / Tool(tool_id) / SubAgent(agent_id) /
/// Interaction(request_id)。
fn fold_key_of(vm: &TuiRenderUnit) -> Option<(FoldKey, FoldState)> {
    match vm {
        TuiRenderUnit::TuiAssistantBubble(b) => {
            let r = b.reasoning.as_ref()?;
            Some((FoldKey::Reasoning(b.message_id.clone()?), r.fold))
        }
        TuiRenderUnit::TuiToolCard(t) => Some((FoldKey::Tool(t.tool_id.clone()), t.fold)),
        TuiRenderUnit::TuiSubAgentGroup(g) => Some((FoldKey::SubAgent(g.agent_id.clone()), g.fold)),
        TuiRenderUnit::TuiAskUserBlock(a) => {
            Some((FoldKey::Interaction(a.request_id.clone()?), a.fold))
        }
        _ => None,
    }
}

/// 对 VM 应用手动折叠覆盖：写 fold + user_modified + 重算 hash（G1）。
/// 调用方必须先写 FOLD_OVERRIDES 覆盖表——快照重建（push_view_models）后
/// 由折叠 pass 依据覆盖表恢复 fold/user_modified，手动选择跨流式保持。
fn apply_fold_override(vm: &mut TuiRenderUnit, fold: FoldState) {
    match vm {
        TuiRenderUnit::TuiAssistantBubble(b) => {
            if let Some(r) = b.reasoning.as_mut() {
                r.fold = fold;
                b.recompute_hash();
            }
        }
        TuiRenderUnit::TuiToolCard(t) => {
            t.fold = fold;
            t.user_modified = true;
            t.recompute_hash();
        }
        TuiRenderUnit::TuiSubAgentGroup(g) => {
            g.fold = fold;
            g.user_modified = true;
            g.recompute_hash();
        }
        TuiRenderUnit::TuiAskUserBlock(a) => {
            a.fold = fold;
            a.user_modified = true;
            a.recompute_hash();
        }
        _ => {}
    }
}

/// [Slice 4 §6.8] 取出 pending 的 interaction block（§6.8）——选项导航/提交
/// 的目标。completed（结果行）不在此列（走折叠切换）。
fn pending_interaction_of(vm: &TuiRenderUnit) -> Option<&TuiAskUserBlock> {
    match vm {
        TuiRenderUnit::TuiAskUserBlock(a) if a.pending => Some(a),
        _ => None,
    }
}

/// [Slice 4 §6.8] interaction option 循环切换（Tab/← 后退、→ 前进；首末回绕）。
/// `count` 调用方已归一化 ≥1。后退在首项回绕到末项（循环语义——浏览器 Tab
/// 直觉；不能用 `saturating_sub`——首项会卡死无法回绕）。
fn cycle_interaction_option(current: usize, count: usize, back: bool) -> usize {
    debug_assert!(count >= 1);
    if back {
        (current + count - 1) % count
    } else {
        (current + 1) % count
    }
}

/// [Slice 4 §6.8] 提交 interaction block 的指定选项（双轨 D5：与弹窗/面板
/// 同一响应通道——HITL_RESPONSE_TX / ASK_USER_RESPONSE_TX；InteractionResolved
/// 结果回写由 ask_user_action / hitl_response 消费者发出）。同时关闭模态层
/// （HITL 弹窗 / AskUser 面板），保持双轨一致。request_id 缺失时 no-op。
fn submit_interaction_option(block: &TuiAskUserBlock, option_index: usize) {
    let Some(id_str) = block.request_id.clone() else {
        return;
    };
    match block.kind {
        InteractionKind::Permission => {
            // D6：HITL 只渲染 [Allow once] [Deny] 两选项（[Always allow] 为
            // 协议依赖项，记入 active spec）。
            let action = if option_index == 0 {
                crate::kit::hitl_response::HitlResponseAction::Approve {
                    request_id_str: id_str,
                }
            } else {
                crate::kit::hitl_response::HitlResponseAction::Reject {
                    request_id_str: id_str,
                }
            };
            if let Some(tx) = crate::kit::atoms::HITL_RESPONSE_TX.get() {
                let _ = tx.send(action);
            }
            crate::kit::popup_overlay::close_popup();
        }
        InteractionKind::AskUser => {
            let label = block.options.get(option_index).cloned().unwrap_or_default();
            let answers = build_inline_answers(&label);
            if let Some(tx) = crate::kit::atoms::ASK_USER_RESPONSE_TX.get() {
                let _ = tx.send(crate::kit::ask_user_action::AskUserResponseAction::Submit {
                    request_id_str: id_str,
                    answers,
                });
            }
            // 关闭面板 + 清 payload（与面板提交路径一致）
            crate::kit::panel_registry::close_panel(crate::app::panel_types::PanelKind::AskUser);
            *crate::kit::atoms::ASK_USER_PENDING.state().write() = None;
            *crate::kit::atoms::ASK_USER_REQUEST_ID.state().write() = None;
        }
    }
}

/// [Slice 4 §6.8] AskUser inline 快速回答的 answers map：首问 = 选中 label，
/// 其余问题空字符串（协议结构完整——单选 string 类型，面板的空答案先例
/// `json!("")`）。从 ASK_USER_PENDING 读首问 id（面板仍打开；防御分支返回
/// 空 map，提交失败不 panic）。
fn build_inline_answers(label: &str) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    if let Some(au) = crate::kit::atoms::ASK_USER_PENDING.state().read().as_ref() {
        for (i, q) in au.questions.iter().enumerate() {
            let val = if i == 0 && !label.is_empty() {
                serde_json::json!(label)
            } else {
                serde_json::json!("")
            };
            map.insert(q.id.clone(), val);
        }
    }
    serde_json::Value::Object(map)
}

/// [Slice 2/4] 对焦点 entry 应用折叠切换（Enter Collapsed↔Expanded /
/// Space → Preview）：写 FOLD_OVERRIDES + 当帧快照 COW set + 重算 hash。
/// 无折叠能力的 entry 消费但不动作（避免误触发送）。
/// 调用方保证 `idx < snapshot.items.len()`（焦点失效已在调用点处理）。
fn apply_fold_toggle(
    snapshot: &mut ViewModelsSnapshot,
    idx: usize,
    next_is_preview: bool,
) -> EventResult {
    let Some((fold_key, current_fold)) = fold_key_of(&snapshot.items[idx]) else {
        // 无折叠能力的 entry（纯文本 assistant / user）：消费但不切换，
        // 避免误触发送。
        return EventResult::Consumed;
    };
    // [Slice 2] §6.7：subagent Enter → 打开详情 pane（不切折叠——subagent
    // 折叠恒 Collapsed 是 §7 表裁决，fold_key_of 不动）；Tool/Reasoning 的
    // Enter 语义不变。写 SELECTED_SUBAGENT_ID 供详情面板按 id 从 VIEW_MODELS
    // 扫描嵌套消息。
    if !next_is_preview && let FoldKey::SubAgent(agent_id) = &fold_key {
        *SELECTED_SUBAGENT_ID.state().write() = Some(agent_id.clone());
        crate::kit::panel_registry::open_panel(crate::app::panel_types::PanelKind::SubAgentDetail);
        return EventResult::Consumed;
    }
    let next = if next_is_preview {
        if current_fold == FoldState::Preview {
            FoldState::Collapsed
        } else {
            FoldState::Preview
        }
    } else if current_fold == FoldState::Collapsed {
        FoldState::Expanded
    } else {
        FoldState::Collapsed
    };
    // 持久覆盖表：快照重建（push_view_models）后由折叠 pass 恢复，
    // 手动选择跨流式/跨 turn 保持（spec §7）。
    FOLD_OVERRIDES.state().write().insert(fold_key, next);
    // 应用到当帧快照（COW set + 重算 hash）
    let mut updated = snapshot.items[idx].clone();
    apply_fold_override(&mut updated, next);
    snapshot.items.set(idx, updated);
    snapshot.generation = snapshot.generation.wrapping_add(1);
    EventResult::Consumed
}

/// 计算可滚动内容的视觉高度。
///
/// 视觉行索引和滚动偏移均为 `usize`；仅终端几何坐标保留 `u16`，避免长消息在
/// 65,535 行处截断而无法滚到底部。
fn total_visual_rows(core_rows: usize, footer_rows: usize, is_loading: bool) -> usize {
    if core_rows == 0 && footer_rows == 0 {
        usize::from(is_loading)
    } else {
        core_rows
            .saturating_add(footer_rows)
            .saturating_add(scroll::SCROLL_PADDING)
    }
}

// ── 渲染性能诊断（PERI_RENDER_TIMING=1 启用）──────────────────────────────

fn render_timing_enabled() -> bool {
    thread_local! {
        static ENABLED: bool = std::env::var("PERI_RENDER_TIMING")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
    }
    ENABLED.with(|&e| e)
}

/// 如果启用诊断，打印阶段耗时。
#[track_caller]
fn trace_phase(phase: &str, start: Instant, detail: Option<&str>) {
    if render_timing_enabled() {
        let elapsed_us = start.elapsed().as_micros();
        let extra = detail.map(|d| format!(" | {d}")).unwrap_or_default();
        tracing::info!(target: "perf.render", "[{phase}] {elapsed_us}μs{extra}");
    }
}

// ── 按 VM 分片的渲染缓存 ──────────────────────────────────────────────────
//
// [Why] 旧版 lines_cache / wrap_map_cache / total_rows_cache 以 (vm_generation, width)
// 为 key，但 push_view_models 每个 token 都 generation += 1，流式期间缓存永远不命中，
// 每个 token 都触发 O(N×W) 的全量 markdown 解析 + wrap_map 重建 + line_count → CPU 拉满。
//
// 现在按 VM 的 content_hash 分片：只有正在流式（hash 变化）的那个 VM 重新解析 markdown
// + 重建 wrap_map，其余 VM 直接 Arc::clone 复用。流式单次成本从 O(N×W) 降至 O(W)。
//
// content_hash 由 build_view_models / TuiAssistantBubble::recompute_hash 维护，
// 已覆盖 text / reasoning.text / reasoning.collapsed / tool duration(secs) 等可变字段。
#[derive(Clone, Default)]
struct VmCacheSlot {
    /// 上次渲染时 VM 的 content_hash。变化时（流式追加 text、折叠/展开 reasoning、
    /// tool duration 跨秒）触发 markdown 重新解析 + wrap_map 重建。
    content_hash: u64,
    /// 上次渲染时的视宽。width 变化（窗口 resize）时 wrap 规则改变，必须重建。
    width: u16,
    /// 上次渲染时的 palette 关键字段哈希。主题切换时 hash 变化 → 强制重建所有 VM 的 markdown 渲染。
    palette_key: u64,
    /// 上次渲染时的 LANG_VERSION。语言切换时递增 → 强制重建（md 复制按钮文本依赖 i18n）。
    lang_key: u64,
    /// 该 VM 解析后的所有 Line（markdown + reasoning + tool card 渲染结果）。
    lines: Arc<Vec<Line<'static>>>,
    /// 该 VM 内部 wrap_map（visual_row 从 0 起）。拼接时累加 visual_offset 和 logical_idx 偏移。
    wrap_map: Arc<Vec<WrappedLineInfo>>,
    /// 该 VM 占据的视觉行数（= wrap_map 末项 visual_end）。
    visual_rows: usize,
    /// [Phase 2] markdown 增量渲染缓存——按文本前缀复用 stable_state，仅处理新增 block。
    /// 仅 AssistantBubble / UserBubble 实际使用；其他 VM 类型保留默认值不消耗资源。
    markdown_cache: crate::kit::markdown::MarkdownRenderCache,
    /// md 复制按钮布局（slot 内逻辑索引 + 列范围）。None = 该 VM 无按钮
    /// （非 AssistantBubble / 空文本 / 宽度不足）。rebuild 时随 lines 重建。
    copy_button: Option<render::CopyButtonInfo>,
    /// [Slice 4 §6.8] pending interaction block 的选项行布局（slot 内逻辑行
    /// 与列区间）。None = 非 pending interaction。rebuild 时随 lines 重建，
    /// 供视口 post-pass 应用「当前项」高亮与点击热区。
    interaction: Option<render::InteractionLayout>,
}

/// md 复制按钮的屏幕点击区域（每帧由渲染 body 构建，点击 handler 实时读取）。
/// [Why] 与 keepgoing 按钮同模式：事件在上帧渲染完成后分发，读取最近一帧的位置；
/// 存屏幕绝对坐标（含 scroll_y / area 偏移换算），handler 无需再查 wrap_map。
struct CopyButtonHit {
    /// 按钮所在屏幕行（绝对坐标）。
    row: u16,
    /// 按钮文本列范围（屏幕绝对坐标，[x_start, x_end)）。
    x_start: u16,
    x_end: u16,
    /// 所属 VM 在 VIEW_MODELS.items 中的索引——点击时读取其 text 复制。
    slot_index: usize,
    /// 渲染时该 VM 的 content_hash——点击时校验索引仍指向同一 VM
    /// （Rewind / Reset 可能增删 items 导致索引错位）。
    vm_hash: u64,
}

/// [Slice 4 §6.8] interaction option 的屏幕点击区域（每帧由渲染 body 构建，
/// 点击 handler 实时读取——与 CopyButtonHit 同模式；事件在上帧渲染完成后
/// 分发，读取最近一帧的位置）。单击 = 提交该选项（按钮语义）。
struct InteractionOptionHit {
    /// 选项所在屏幕行（绝对坐标）。
    row: u16,
    /// 选项文本列范围（屏幕绝对坐标，[x_start, x_end)；垂直排列/超宽时
    /// 整行命中）。
    x_start: u16,
    x_end: u16,
    /// 所属 VM 在 VIEW_MODELS.items 中的索引——点击时校验仍指向同一 VM。
    slot_index: usize,
    /// 渲染时该 VM 的 content_hash——点击时校验（Rewind / Reset 索引错位防御）。
    vm_hash: u64,
    /// 选项索引——提交时选择哪个 option。
    option_index: usize,
}

// ── 组件 ──────────────────────────────────────────────────────────────────

#[component]
pub fn MessageArea(props: &MessageAreaProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    tracing::trace!(target: "frozen_diag", "MessageArea: update/body called");
    let view_models = hooks.use_atom(&VIEW_MODELS);
    let acp_state = hooks.use_atom(&crate::kit::atoms::ACP_STATE);
    let todo_atom = hooks.use_atom(&crate::kit::atoms::TODO_ITEMS);
    hooks.use_atom(&LANG_VERSION);
    // 订阅 PALETTE_ATOM：主题切换时触发 MessageArea 重渲染，
    // 配合 palette_key 使 vm_caches 失效，确保 markdown 色值随主题更新。
    let _palette = hooks.use_atom(&PALETTE_ATOM);
    let current_palette_key = palette_markdown_key(&_palette.read());
    // 订阅 TERMINAL_CAPS：NO_COLOR 时对可见行做颜色剥离（§12，G3 视口级 pass）。
    // 启动时探测一次后不再变化；订阅仅为语义完整（切换不重渲染也无副作用）。
    let caps = hooks.use_atom(&crate::kit::atoms::TERMINAL_CAPS);
    let strip_color = !caps.read().color;

    let snapshot = view_models.read();
    let todo_items = todo_atom.read().clone();
    let is_loading = acp_state.read().is_loading;

    // [Slice 3] Transcript 统一水平网格（§3.1）——content 列宽来自 SessionColumn。
    let grid = props.grid;

    // [PERI_RENDER_TIMING] 帧计时起点
    let frame_t0 = if render_timing_enabled() {
        Some(Instant::now())
    } else {
        None
    };

    let vm_generation = snapshot.generation;

    // ── 按 VM 分片的渲染缓存 ──────────────────────────────────────────────────
    // vm_caches 与 snapshot.items 长度对齐，每个 slot 缓存一个 VM 的 lines + wrap_map。
    // [TRAP] write_no_update 必须用——ratatui-kit ReactiveMutRef::Drop 无条件 wake()，
    // render body 内 write 会自激回路 100% CPU。
    let vm_caches: State<Vec<VmCacheSlot>> = hooks.use_state(Vec::new);

    // ── Footer 行预计算：必须在 empty 分支之前调用，确保所有 hook 顺序一致 ──
    // keepgoing 防抖：KEEPGOING_BLOCKED_UNTIL 内的时间未过期 → 按钮禁用样式渲染
    let keepgoing_blocked = crate::kit::atoms::KEEPGOING_BLOCKED_UNTIL
        .state()
        .read()
        .is_some_and(|until| Instant::now() < until);
    // 消息区位置追踪 + 视宽：build_footer_lines 需要 vis_width 判断按钮是否
    // 超宽换行（m4：换行后点击区域与实际渲染位置错位，超宽时跳过按钮渲染）。
    let area_hook = hooks.use_hook(MsgAreaTracker::new);
    let area_rect = area_hook.rect;
    let vis_width = area_rect
        .map(|r| r.width.saturating_sub(1))
        .unwrap_or(grid.total_width() as u16)
        .max(1);
    let (footer_lines, keepgoing_layout, footer_has_content) = build_footer_lines(
        &mut hooks,
        is_loading,
        &todo_items,
        keepgoing_blocked,
        vis_width,
    );
    // keepgoing 按钮屏幕点击区域 (y, x_start, width)，每帧更新，点击 handler 实时读取
    // （handler 闭包捕获的是 State 句柄而非帧快照，滚动后坐标仍正确）
    let keepgoing_rect = hooks.use_state(|| Option::<(u16, u16, u16)>::None);
    // md 复制按钮屏幕点击区域（每帧更新，点击 handler 实时读取——同上）
    let copy_buttons = hooks.use_state(Arc::<Vec<CopyButtonHit>>::default);

    let empty = snapshot.items.is_empty() && !is_loading && todo_items.is_empty();
    // [Why has_content 而非 !footer_lines.is_empty()] footer 常驻渲染后恒非空
    // （idle 态含静止 spinner 占位行），空态若据此判定会让 Welcome 页面被
    // footer 占位行污染；仅当有实质内容（summary/todo）时才在 Welcome 下展示。
    let brewed_lines = if empty && footer_has_content {
        Some(footer_lines.clone())
    } else {
        None
    };

    let scroll_state = hooks.use_state(scroll::ScrollPos::default);
    let prev_items_len = hooks.use_state(|| 0usize);
    let _prev_is_loading = hooks.use_state(|| false);
    let scroll_throttle = hooks.use_state(ScrollThrottle::default);
    let _todo_hash = hash_todo_items(&todo_items);

    // ── 文本选区 ──
    let text_sel = hooks.use_state(TextSelection::default);
    let selection_down_pos = hooks.use_state(|| Option::<(usize, u16)>::None);
    let drag_throttle = hooks.use_state(DragThrottle::default);

    // 滚动条 fields state（hook 通过引用读取，避免 borrow 冲突）
    let scrollbar_fields = hooks.use_state(ScrollbarFields::default);
    hooks.use_hook(move || ScrollbarHook {
        fields: scrollbar_fields,
    });
    // 滚动条 thumb 拖拽状态（点击/拖拽事件处理器读写）
    let scrollbar_drag = hooks.use_state(ScrollbarDragState::default);
    // [Slice 2] §8.1 `↓ New output` 指示器屏幕点击区域 (y, x_start, x_end)，
    // 每帧由渲染 body 更新（write_no_update），点击 handler 实时读取——与
    // keepgoing / md 复制按钮同模式（事件在上帧渲染完成后分发）。
    let new_output_rect = hooks.use_state(|| Option::<(u16, u16, u16)>::None);
    // [Slice 4 §6.8] interaction option 点击热区（每帧由渲染 body 更新，
    // 点击 handler 实时读取——同 CopyButtonHit 模式）。
    let interaction_rects = hooks.use_state(Arc::<Vec<InteractionOptionHit>>::default);
    // [Slice 4 §6.8] interaction option 键盘焦点（entry 焦点内部态，不新增
    // FocusLayer——option 是 entry 焦点内部状态，仲裁仍走 message_nav_accepts）。
    // 焦点移动到其他 entry 时重置为 0（下方 handler）。
    let interaction_option = hooks.use_state(|| 0usize);
    // [Slice 4 §6.8]「等待时锚定此 block」：pending interaction block 的 slot
    // index（每帧派生：扫描快照，pending 完成 → 自然清除）。
    let anchor_slot_state = hooks.use_state(|| Option::<usize>::None);

    let vis_height = area_rect.map(|r| r.height).unwrap_or(60).max(1);

    // ── 遍历每个 VM，按 (content_hash, vis_width) 命中判断 ──
    // [Why] 流式期间只有最后一个 AssistantBubble 的 content_hash 变化（text 累积），
    // 其余 committed VM hash 完全稳定——直接复用 Arc<Vec<Line>> 和 Arc<Vec<WrappedLineInfo>>。
    // 单次成本从 O(N×W) 降至 O(W)。
    //
    // [Opt B] hash-only clone：只收集 content_hash (u64)，避免每帧全量 clone TuiRenderUnit。
    // rebuild 阶段按需通过 snapshot 重读 VM 数据（流式期间只追加，索引有效）。
    //
    // [TRAP] 必须先提取 hashes 再 drop(snapshot)，否则 vm_caches.write_no_update 与
    // view_models read guard 冲突。
    //
    // [Fix §15] [Slice 4 §6.8] pending interaction block 的 slot index 与
    // item_hashes 同一次扫描派生（原独立 O(N) 循环合并——每帧至多一次全量
    // 遍历；同一时刻至多一个 pending block（模态互斥），break 语义保留）。
    let mut anchor_slot: Option<usize> = None;
    let item_hashes: Vec<u64> = snapshot
        .items
        .iter()
        .enumerate()
        .map(|(i, vm)| {
            if anchor_slot.is_none() && matches!(vm, TuiRenderUnit::TuiAskUserBlock(a) if a.pending)
            {
                anchor_slot = Some(i);
            }
            vm.content_hash()
        })
        .collect();
    let items_len = item_hashes.len();
    // [Slice 4 §6.8] pending 完成（结果回写 pending=false）后扫描不到 →
    // anchor 自然清除。write_no_update 不触发自激重渲染；block 完成时
    // push_view_models 的 generation 变化会触发 auto_follow effect。
    if *anchor_slot_state.read() != anchor_slot {
        *anchor_slot_state.write_no_update() = anchor_slot;
    }
    drop(snapshot);

    // 同步 vm_caches 长度对齐 item_hashes
    if vm_caches.read().len() != items_len {
        vm_caches
            .write_no_update()
            .resize(items_len, VmCacheSlot::default());
    }

    // 第一阶段：检测哪些 slot 需要 rebuild（content_hash 或 vis_width 变化）
    // [Opt B] 用 item_hashes (Vec<u64>) 替代 items_vec 迭代，避免持有 TuiRenderUnit clone。
    let rebuild_indices: Vec<usize> = {
        let caches_read = vm_caches.read();
        item_hashes
            .iter()
            .enumerate()
            .filter_map(|(i, hash)| {
                let slot = &caches_read[i];
                if slot.content_hash != *hash
                    || slot.width != vis_width
                    || slot.palette_key != current_palette_key
                    || slot.lang_key != LANG_VERSION.get()
                {
                    Some(i)
                } else {
                    None
                }
            })
            .collect()
    };
    // [PERI_RENDER_TIMING] hash 比对耗时
    let t_hash = Instant::now();
    trace_phase(
        "hash+detect",
        frame_t0.unwrap_or(t_hash),
        Some(&format!(
            "items={items_len}, rebuilds={}",
            rebuild_indices.len()
        )),
    );

    // 第二阶段：rebuild 需要更新的 slot——只有这些 VM 调用 vm_to_lines + build_wrap_map
    // [Phase 2] markdown_cache 在 slot 内部跨 rebuild 复用：即使 VM hash 变化（text 追加 token），
    // cache 仍能命中上次 stable_text 前缀，仅处理新增 block。
    // [Borrow] 单次 write_no_update 持锁整个 rebuild 循环——ratatui-kit State 仅在
    // render body 单线程访问，无锁竞争。直接可变借用 slot.markdown_cache，避免 clone
    // ConvertState（含 Vec<Line>，clone 成本高）。
    //
    // [Opt B] 仅 rebuild_indices 非空时才重新 read snapshot 获取 VM 引用。
    // safe_len 防御 TOCTOU 索引越界（Rewind/Reset 可能缩短 items）。
    if !rebuild_indices.is_empty() {
        let snapshot2 = view_models.read();
        let safe_len = snapshot2.items.len().min(items_len);
        let mut caches = vm_caches.write_no_update();
        for i in &rebuild_indices {
            if *i >= safe_len {
                continue; // TOCTOU 防御：跳过不可用索引，下帧重新同步
            }
            let vm = &snapshot2.items[*i];
            let vm_hash = vm.content_hash();
            let slot = &mut caches[*i];
            let (lines, copy_button, interaction) =
                vm_to_lines_cached(vm, &grid, &mut slot.markdown_cache, true);
            let lines = Arc::new(lines);
            let (_, wm) = build_wrap_map(&lines, vis_width);
            let visual_rows = wm.last().map(|e| e.visual_end).unwrap_or(0);
            slot.content_hash = vm_hash;
            slot.width = vis_width;
            slot.palette_key = current_palette_key;
            slot.lang_key = LANG_VERSION.get();
            slot.lines = lines;
            slot.wrap_map = Arc::new(wm);
            slot.visual_rows = visual_rows;
            slot.copy_button = copy_button;
            slot.interaction = interaction;
        }
    }
    // [PERI_RENDER_TIMING] rebuild 耗时（仅 rebuild_indices 非空时有意义）
    let t_rebuild = Instant::now();
    if !rebuild_indices.is_empty() {
        trace_phase(
            "rebuild",
            t_hash,
            Some(&format!("{} slots", rebuild_indices.len())),
        );
    }

    // 第三阶段：拼接 concat_wrap_map（累加 visual_offset 和 logical_idx）。
    // [Scheme D] 不再构建全量 core_lines——仅收集每个 slot 的 Arc<Vec<Line>> 引用
    // 和 slot_offsets（累积偏移）。视口循环按需从 slot 中提取行。
    // [Why] per-frame clone 全量 core_lines 是 Phase 1 之后的首要瓶颈。
    let mut core_total_visual_rows: usize = 0;
    let mut total_logical_lines: usize = 0;
    let mut slot_arcs: Vec<Arc<Vec<Line<'static>>>> = Vec::new();
    let mut slot_offsets: Vec<usize> = Vec::new();
    // 每个 slot 在拼接后的全量视觉行中的起始偏移（供 md 复制按钮换算屏幕坐标）。
    let mut slot_visual_starts: Vec<usize> = Vec::new();
    // 每个 slot 的视觉行数（与 slot_visual_starts 同长度）——anchor 视觉范围
    // 用 `start + rows` 直接算出，避免每帧对 concat_wrap_map 做第二次全量扫描。
    let mut slot_visual_rows: Vec<usize> = Vec::new();
    let concat_wrap_map: Vec<WrappedLineInfo> = {
        let caches_read = vm_caches.read();
        slot_arcs.reserve(caches_read.len());
        slot_offsets.reserve(caches_read.len());
        slot_visual_starts.reserve(caches_read.len());
        slot_visual_rows.reserve(caches_read.len());
        let mut slots: Vec<(&[WrappedLineInfo], usize, usize)> =
            Vec::with_capacity(caches_read.len());
        for (slot_index, slot) in caches_read.iter().enumerate() {
            let lines_start = total_logical_lines;
            slot_offsets.push(lines_start);
            total_logical_lines += slot.lines.len();
            slot_arcs.push(Arc::clone(&slot.lines));
            slots.push((&slot.wrap_map, lines_start, slot_index));
            slot_visual_starts.push(core_total_visual_rows);
            slot_visual_rows.push(slot.visual_rows);
            core_total_visual_rows += slot.visual_rows;
        }
        concat_wrap_maps(&slots)
    };
    let slot_arcs_arc: Arc<Vec<Arc<Vec<Line<'static>>>>> = Arc::new(slot_arcs);
    let slot_offsets_arc: Arc<Vec<usize>> = Arc::new(slot_offsets);
    let concat_wrap_map_arc: Arc<Vec<WrappedLineInfo>> = Arc::new(concat_wrap_map);
    // [PERI_RENDER_TIMING] concat 阶段耗时
    let num_slots = slot_arcs_arc.len();

    // [Slice 4 §6.8] anchor 的视觉行范围（core 行）：slot 起始偏移 + slot 内
    // 视觉行数。供 auto_follow 的 anchor 分支对齐视口到 block 底部。
    // 每帧由 anchor_slot 派生（resize 后按新快照/wrap_map 自动重算）。
    // [Fix §15] O(1) 查表：旧实现每帧对 concat_wrap_map 全量 filter 求最大
    // visual_end（O(total wrapped lines) 第二次全量遍历）——slot.visual_rows
    // 即 wrap_map 末项 visual_end（rebuild 时同源写入），与旧语义完全一致。
    let anchor_visual_range: Option<(usize, usize)> = anchor_slot_state.read().and_then(|slot| {
        let start = *slot_visual_starts.get(slot)?;
        let rows = *slot_visual_rows.get(slot)?;
        if rows == 0 {
            return None; // 空 slot（无 wrap 条目）→ 无锚定（与旧 max() 语义一致）
        }
        Some((start, start + rows))
    });
    let t_concat = Instant::now();
    trace_phase(
        "concat",
        t_rebuild,
        Some(&format!(
            "slots={num_slots}, logic_lines={total_logical_lines}, vis_rows={core_total_visual_rows}"
        )),
    );

    // ── 总视觉行数 = core + footer ──
    // [Why] 分片缓存命中后 core_total_visual_rows 已是 sum(slot.visual_rows)，无需 line_count。
    // footer 通常几行，单独 build_wrap_map 成本可忽略。
    let footer_visual_rows: usize = if !empty && !footer_lines.is_empty() {
        let (_, footer_map) = build_wrap_map(&footer_lines, vis_width);
        footer_map.last().map(|e| e.visual_end).unwrap_or(0)
    } else {
        0
    };
    // [Padding] 追加 SCROLL_PADDING 行空白滚动空间，减少流式输出期间因新行到达
    // 导致的视口抖动。这 2 行不计入实际渲染内容，仅影响 max_scroll / content_length；
    // 吸底跟随恢复判定会扣除它们（scroll::should_follow_after_user_scroll）。
    let total_visual_rows =
        total_visual_rows(core_total_visual_rows, footer_visual_rows, is_loading);

    // ── keepgoing 按钮点击（footer summary 行右侧）──
    // [Why] 必须注册在 scroll handler 之前：两者同 Global+High，同优先级按注册序分发，
    // scroll::handle_event 对消息区内 Down(Left) 一律 Consumed（文本选中起点）——
    // 若在其后注册，按钮点击会被 scroll handler 截断、永远收不到。
    // 命中 → Consumed（scroll handler 不处理该点击，不会误设选区起点）；
    // 未命中 → Ignored（滚动/选区逻辑照常）。
    // [TRAP] 闭包捕获 State 句柄（keepgoing_rect）而非帧快照——每帧 write_no_update
    // 更新 rect，滚动/布局变化后坐标仍准确；事件在上帧渲染完成后分发，读取的
    // 是最近一帧的按钮位置。
    {
        hooks.use_event_handler(EventScope::Global, EventPriority::High, move |event| {
            let Event::Mouse(mouse) = event else {
                return EventResult::Ignored;
            };
            if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
                return EventResult::Ignored;
            }
            // 弹窗/面板遮挡时不响应（与 status_bar / scroll handler 一致）
            if mouse_router::is_occluded() {
                return EventResult::Ignored;
            }
            // 命中检测：点击坐标落在按钮屏幕区域内 (y, x_start, width)
            // [Why 先于防抖] 防抖期内点击禁用按钮也应 Consumed——否则事件落到
            // scroll handler 的文本选区逻辑（消息区内 Down(Left) 设置选区锚点）。
            let Some((by, bx, bw)) = *keepgoing_rect.read() else {
                return EventResult::Ignored;
            };
            let (x, y) = (mouse.column, mouse.row);
            if y != by || x < bx || x >= bx.saturating_add(bw) {
                return EventResult::Ignored;
            }
            // 防抖：防抖期内按钮渲染为禁用样式，点击被吞掉但不触发提交
            let now = Instant::now();
            let blocked = KEEPGOING_BLOCKED_UNTIL
                .state()
                .read()
                .is_some_and(|until| now < until);
            if blocked {
                return EventResult::Consumed;
            }
            // 触发 keepgoing 提交：发送空白 user prompt（服务端不插入 user 消息，仅继续 loop）
            if let Some(tx) = SUBMIT_TX.get() {
                let _ = tx.send(SubmitRequest::KeepGoing);
            }
            // 防抖：冷却期内按钮禁用（渲染为 muted 样式，见 build_footer_lines）
            *KEEPGOING_BLOCKED_UNTIL.state().write() = Some(now + KEEPGOING_DEBOUNCE);
            // 防抖到期后清除阻塞并 bump 心跳触发重渲染，恢复可点击样式
            tokio::spawn(async move {
                tokio::time::sleep(KEEPGOING_DEBOUNCE).await;
                *KEEPGOING_BLOCKED_UNTIL.state().write() = None;
                RENDER_HEARTBEAT.set(RENDER_HEARTBEAT.get().wrapping_add(1));
            });
            EventResult::Consumed
        });
    }

    // ── md 复制按钮点击（复制整条 AI 回复的原始 markdown）──
    // [Why] 注册顺序与 keepgoing 相同：必须在 scroll handler 之前——scroll::handle_event
    // 对消息区内 Down(Left) 一律 Consumed（文本选中起点），在其后注册收不到点击。
    // 命中 → Consumed（滚动/选区逻辑不处理该点击，不会误设选区起点）；
    // 未命中 → Ignored（滚动/选区逻辑照常）。
    // [Why 每次渲染重建] ratatui-kit 的 use_event_handler 闭包每帧重新注册（当帧值），
    // copy_buttons State 由渲染 body 后部 write_no_update 更新——事件分发时读到的
    // 是最近一帧的按钮位置（与 keepgoing 一致）。
    {
        hooks.use_event_handler(EventScope::Global, EventPriority::High, move |event| {
            let Event::Mouse(mouse) = event else {
                return EventResult::Ignored;
            };
            if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
                return EventResult::Ignored;
            }
            // 弹窗/面板遮挡时不响应（与 status_bar / scroll handler 一致）
            if mouse_router::is_occluded() {
                return EventResult::Ignored;
            }
            let (x, y) = (mouse.column, mouse.row);
            let hits = copy_buttons.read();
            let Some(hit) = hits
                .iter()
                .find(|h| y == h.row && x >= h.x_start && x < h.x_end)
            else {
                return EventResult::Ignored;
            };
            // 读取最新 VM 文本：校验 hash 防 Rewind/Reset 后索引错位。
            // [LOW-5] assistant 用稳定身份 hash 比对（排除时变 duration——
            // 运行中 bubble 的 content_hash 每秒漂移，跨秒点击偶发拒绝）；
            // 其余类型沿用 content_hash。
            let snapshot = view_models.read();
            let matched = snapshot
                .items
                .get(hit.slot_index)
                .is_some_and(|vm| match vm {
                    TuiRenderUnit::TuiAssistantBubble(b) => {
                        TuiAssistantBubble::stable_identity_hash(&b.text, b.reasoning.as_ref())
                            == hit.vm_hash
                    }
                    _ => vm.content_hash() == hit.vm_hash,
                });
            let text = if matched {
                match &snapshot.items[hit.slot_index] {
                    TuiRenderUnit::TuiAssistantBubble(d) => Some(d.text.clone()),
                    _ => None,
                }
            } else {
                None
            };
            drop(snapshot);
            if let Some(text) = text {
                copy_to_clipboard(text.clone());
                mark_copy_message(text.chars().count());
            }
            // 命中按钮（即使 VM 不匹配）也 Consumed——防止点击落到文本选区逻辑
            EventResult::Consumed
        });
    }

    // ── 鼠标事件处理（滚动 + 文本拖拽选中复制）──
    // [TRAP] event_handler 闭包必须是 'static → 必须 move。但 concat_wrap_map_arc /
    // slot_arcs_arc / slot_offsets_arc 后续视口裁剪也要用。Arc::clone 是 O(1) 引用计数，
    // ── 吸底自动跟随 ──
    let last_scrolled_at = hooks.use_state(|| 0usize);
    // 粘性吸底开关：默认跟随；用户向上滚动即退出（浏览模式），滚回底部才恢复。
    let follow_bottom = hooks.use_state(|| true);

    // ── `↓ New output` 指示器点击（§8.1：滚回底部并恢复跟随）──
    // [Why 注册顺序] 必须注册在 scroll handler（下方）之前：scroll::handle_event
    // 对消息区内 Down(Left) 一律 Consumed（文本选中起点）——在其后注册收不到点击。
    // 命中 → Consumed（滚动/选区逻辑不处理该点击）；未命中 → Ignored。
    // [TRAP] 闭包捕获 State 句柄（new_output_rect）而非帧快照——每帧
    // write_no_update 更新 rect，滚动/布局变化后坐标仍准确。
    {
        let new_output_rect_state = new_output_rect;
        let follow_state = follow_bottom;
        let scroll_state_for_indicator = scroll_state;
        hooks.use_event_handler(EventScope::Global, EventPriority::High, move |event| {
            let Event::Mouse(mouse) = event else {
                return EventResult::Ignored;
            };
            if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
                return EventResult::Ignored;
            }
            // 弹窗/面板遮挡时不响应（与 keepgoing / scroll handler 一致）
            if mouse_router::is_occluded() {
                return EventResult::Ignored;
            }
            let Some((ry, rx_start, rx_end)) = *new_output_rect_state.read() else {
                return EventResult::Ignored;
            };
            let (x, y) = (mouse.column, mouse.row);
            if y != ry || x < rx_start || x >= rx_end {
                return EventResult::Ignored;
            }
            // 恢复跟随 + 滚到底（渲染每帧 clamp scroll_to_bottom 的 usize::MAX
            // 哨兵到当帧 max_scroll——与 End 键同一路径）。
            *follow_state.write() = true;
            scroll_state_for_indicator
                .write_no_update()
                .scroll_to_bottom();
            EventResult::Consumed
        });
    }

    // ── [Slice 4 §6.8] interaction option 点击（提交该选项，按钮语义）──
    // [Why 注册顺序] 与 keepgoing/md 复制/new output 一致：必须在 scroll
    // handler 之前——scroll::handle_event 对消息区内 Down(Left) 一律 Consumed
    // （文本选中起点），在其后注册收不到点击。命中 → Consumed；未命中 → Ignored。
    {
        let interaction_rects_state = interaction_rects;
        hooks.use_event_handler(EventScope::Global, EventPriority::High, move |event| {
            let Event::Mouse(mouse) = event else {
                return EventResult::Ignored;
            };
            if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
                return EventResult::Ignored;
            }
            // 弹窗/面板遮挡时不响应（与 keepgoing / scroll handler 一致）
            if mouse_router::is_occluded() {
                return EventResult::Ignored;
            }
            let (x, y) = (mouse.column, mouse.row);
            let hits = interaction_rects_state.read();
            let Some(hit) = hits
                .iter()
                .find(|h| y == h.row && x >= h.x_start && x < h.x_end)
            else {
                return EventResult::Ignored;
            };
            // 校验 VM 身份（Rewind/Reset 索引错位防御）——与 md 复制按钮同模式。
            let vm_guard = view_models.read();
            let block = vm_guard
                .items
                .get(hit.slot_index)
                .filter(|vm| vm.content_hash() == hit.vm_hash)
                .and_then(pending_interaction_of)
                .cloned();
            drop(vm_guard);
            if let Some(block) = block {
                submit_interaction_option(&block, hit.option_index);
            }
            // 命中（即使 VM 不匹配）也 Consumed——防止点击落到文本选区逻辑
            EventResult::Consumed
        });
    }

    // ── entry 单击展开（仅首行 header；与键盘 Enter 同语义）──
    // [Why 注册顺序] 必须在 scroll handler 之前：scroll::handle_event 对消息区内
    // Up(Left) 也会消费（选区复制/清锚点），在其后注册收不到单击。放在
    // interaction option（Down）之后即可——两者事件类型不重叠。
    // [语义] 单击（Down+Up 无 Drag、坐标容差内）落在 entry 首行 →
    // 设置 entry 焦点 + 折叠切换：tool/reasoning/subagent/completed interaction
    // toggle（写 FOLD_OVERRIDES，与键盘 Enter 一致）；subagent 打开详情面板；
    // pending interaction 首行仅聚焦不提交（键盘 Enter 的提交是明确按键语义）。
    // 未命中（拖拽释放/滚动条列/非首行/坐标外）→ Ignored，选区逻辑照常。
    // entry_focus 声明必须在本 handler 之前（闭包捕获），hook 顺序每次渲染一致。
    let entry_focus = hooks.use_state(|| Option::<usize>::None);
    {
        let wrap_map_for_click = Arc::clone(&concat_wrap_map_arc);
        let slot_offsets_for_click = Arc::clone(&slot_offsets_arc);
        hooks.use_event_handler(EventScope::Global, EventPriority::High, move |event| {
            let Event::Mouse(mouse) = event else {
                return EventResult::Ignored;
            };
            if mouse.kind != MouseEventKind::Up(MouseButton::Left) {
                return EventResult::Ignored;
            }
            // 弹窗/面板遮挡时不响应（与 keepgoing / scroll handler 一致）
            if mouse_router::is_occluded() {
                return EventResult::Ignored;
            }
            let Some(area) = area_rect else {
                return EventResult::Ignored;
            };
            if mouse.row < area.y || mouse.row >= area.y.saturating_add(area.height) {
                return EventResult::Ignored;
            }
            // 滚动条列：scrollbar Up 分支负责 thumb 释放，不参与 entry 点击
            if scroll::is_scrollbar_column(mouse.column, area) {
                return EventResult::Ignored;
            }
            // 拖拽释放（选区复制）放行给 scroll::handle_event 的 Up 分支
            if text_sel.read().dragging {
                return EventResult::Ignored;
            }
            // 单击判定：Down 锚点 + 手抖容差（无 Drag = 未移动；容差防事件丢失）
            let Some(down) = *selection_down_pos.read() else {
                return EventResult::Ignored;
            };
            let up = (usize::from(mouse.row), mouse.column);
            if !scroll::is_click(down, up) {
                return EventResult::Ignored;
            }
            let scroll_y = scroll_state.read().offset();
            let visual_row = up.0.saturating_sub(usize::from(area.y)) + scroll_y;
            let Some((slot, 0)) = scroll::entry_click_target(
                &wrap_map_for_click,
                &slot_offsets_for_click,
                visual_row,
            ) else {
                return EventResult::Ignored;
            };
            // ── 命中 entry 首行：设焦点 + 折叠动作 ──
            // 与键盘 Alt+Up/Down 一致：焦点可落在任意 entry，FOCUSED_ENTRY_KEY
            // 仅 foldable 有值；重置 interaction option 到首项。
            tracing::trace!(target: "frozen_diag", slot, "click: hit entry, setting focus");
            *entry_focus.write() = Some(slot);
            *interaction_option.write() = 0;
            // 持 VIEW_MODELS 写锁期间不再读其他可能被同一帧写入的 atom
            //（FOLD_OVERRIDES / SELECTED_SUBAGENT_ID 是独立锁）——键盘同模式。
            tracing::trace!(target: "frozen_diag", slot, "click: acquiring VIEW_MODELS write lock");
            let vm_state_ref = VIEW_MODELS.state();
            let mut snapshot = vm_state_ref.write();
            tracing::trace!(target: "frozen_diag", slot, "click: got VIEW_MODELS write lock");
            if slot >= snapshot.items.len() {
                // 快照缩短（reset/rewind）——焦点失效，退出导航（键盘同模式）
                *entry_focus.write() = None;
                *FOCUSED_ENTRY_KEY.state().write() = None;
                return EventResult::Consumed;
            }
            let focused_key = snapshot
                .items
                .get(slot)
                .and_then(fold_key_of)
                .map(|(k, _)| k);
            *FOCUSED_ENTRY_KEY.state().write() = focused_key;
            // pending interaction：Enter 语义是提交 option（鼠标不承担）；
            // 首行点击仅聚焦，不提交不折叠。
            if pending_interaction_of(&snapshot.items[slot]).is_some() {
                return EventResult::Consumed;
            }
            // 点击 = 取消选区语义（与 keepgoing / md 复制按钮点击一致）
            text_sel.write().clear();
            let result = apply_fold_toggle(&mut snapshot, slot, false);
            tracing::trace!(target: "frozen_diag", slot, "click: handler exit");
            result
        });
    }

    // 闭包持 clone，原值继续在 render body 内用。
    // [Why 位置] 必须声明在 follow_bottom 之后（闭包捕获），且所有 hook 每次渲染
    // 以相同相对顺序调用——use_event_handler 只是占顺序槽，位置调整无状态错位。
    {
        let wrap_map_for_closure = Arc::clone(&concat_wrap_map_arc);
        let slot_arcs_for_closure = Arc::clone(&slot_arcs_arc);
        let slot_offsets_for_closure = Arc::clone(&slot_offsets_arc);
        let view_models_for_closure = view_models;
        let grid_for_closure = grid;
        hooks.use_event_handler(EventScope::Global, EventPriority::High, move |event| {
            // [D3 §9] 语义复制：事件时点读快照 VM 列表（im::Vector clone O(1)，
            // 只读不改——与 parking_lot 读锁安全共存；选区提取需要 VM 类型
            // 分派语义文本，不能只靠已渲染行）。
            let vms_snapshot = view_models_for_closure.read().items.clone();
            scroll::handle_event(
                &event,
                area_rect,
                vis_width,
                &scroll_state,
                &scroll_throttle,
                &text_sel,
                &selection_down_pos,
                &drag_throttle,
                &wrap_map_for_closure,
                &slot_arcs_for_closure,
                &slot_offsets_for_closure,
                &scrollbar_fields,
                &scrollbar_drag,
                &follow_bottom,
                Some(&vms_snapshot),
                Some(grid_for_closure),
            )
        });
    }

    // ── entry 焦点导航（Slice 2 键盘语义层；selection border 视觉归 Slice 3）──
    // Alt+Up/Down 移动 entry 焦点；焦点激活时 Enter 切 Collapsed/Expanded、
    // Space 切 Preview（写 FOLD_OVERRIDES + user_modified）；Esc 退出导航。

    // 键盘：Alt+Up/Down 移焦点；Enter/Space 切折叠（写覆盖表 + 当帧快照）；
    // Tab/←/→ 切换 pending interaction 选项、Enter 提交（§6.8）；
    // Esc 单层取消（退出导航）。仲裁见 focus_router::message_nav_accepts。
    {
        let focus_state = entry_focus;
        let option_state = interaction_option;
        hooks.use_event_handler(EventScope::Global, EventPriority::High, move |event| {
            let Event::Key(key) = event else {
                return EventResult::Ignored;
            };
            if key.kind != KeyEventKind::Press {
                return EventResult::Ignored;
            }
            if mouse_router::is_occluded() {
                return EventResult::Ignored;
            }
            // Esc：仅焦点激活时消费（单层取消，退出导航）；未激活时放行给
            // root handler（双击 Esc → Rewind 等既有语义不受影响）。
            if key.code == KeyCode::Esc
                && focus_state.read().is_some()
                && matches!(
                    focus_router::active_layer(),
                    focus_router::FocusLayer::Input
                )
            {
                *focus_state.write() = None;
                // [§7 免疫] 焦点清除 → 免疫键同步清除（分组 pass 恢复自动合并）。
                *FOCUSED_ENTRY_KEY.state().write() = None;
                return EventResult::Consumed;
            }
            let focused = focus_state.read().is_some();
            if !focus_router::message_nav_accepts(&key, focused) {
                return EventResult::Ignored;
            }
            let alt = key.modifiers.contains(KeyModifiers::ALT);
            match key.code {
                KeyCode::Up | KeyCode::Down if alt => {
                    let items_len = VIEW_MODELS.state().read().items.len();
                    let next = move_entry_focus(
                        items_len,
                        focus_state.read().as_ref().copied(),
                        if key.code == KeyCode::Up { -1 } else { 1 },
                    );
                    *focus_state.write() = next;
                    // [§7 免疫] 焦点移动 → 免疫键同步更新（身份键，非索引——
                    // 快照重建后索引漂移不影响判定）。焦点落在无折叠能力
                    // entry（user/assistant/group）时置 None（分组只涉及工具）。
                    let focused_key = next.and_then(|i| {
                        VIEW_MODELS
                            .state()
                            .read()
                            .items
                            .get(i)
                            .and_then(fold_key_of)
                            .map(|(k, _)| k)
                    });
                    *FOCUSED_ENTRY_KEY.state().write() = focused_key;
                    // 焦点移动到其他 entry——重置 interaction option 到首项
                    *option_state.write() = 0;
                    EventResult::Consumed
                }
                // [Slice 4 §6.8] Tab/←/→：焦点在 pending interaction block 时
                // 切换 option（局部状态，不新增 FocusLayer）；非 interaction 时
                // Ignored 放行（Tab 继续传给输入区——消息区不独占）。
                KeyCode::Tab | KeyCode::Left | KeyCode::Right
                    if key.modifiers == KeyModifiers::NONE =>
                {
                    // 读当前快照判断焦点 entry 类型（只读；无写锁）
                    let vm_guard = VIEW_MODELS.state();
                    let items = &vm_guard.read().items;
                    let idx = *focus_state.read();
                    let block = idx
                        .and_then(|i| items.get(i))
                        .and_then(pending_interaction_of);
                    let Some(block) = block else {
                        return EventResult::Ignored;
                    };
                    let opt_count = block.options.len().max(1);
                    let opt = *option_state.read();
                    // Tab/← 后退、→ 前进；首末回绕（循环语义，浏览器 Tab 直觉）
                    let next_opt = cycle_interaction_option(
                        opt,
                        opt_count,
                        matches!(key.code, KeyCode::Left | KeyCode::Tab),
                    );
                    *option_state.write() = next_opt;
                    EventResult::Consumed
                }
                KeyCode::Enter | KeyCode::Char(' ') => {
                    let next_is_preview = key.code == KeyCode::Char(' ');
                    // 读当前快照：对焦点 entry 应用切换。持 VIEW_MODELS 写锁期间
                    // 不再读其他可能被同一帧写入的 atom（FOLD_OVERRIDES 是独立锁）。
                    // [TRAP] state() 必须绑定变量——临时值在语句末释放会导致
                    // ReactiveMutRef::Drop 在借用期间运行（E0716）。
                    tracing::trace!(target: "frozen_diag", "enter: acquiring VIEW_MODELS write lock");
                    let vm_state_ref = VIEW_MODELS.state();
                    let mut snapshot = vm_state_ref.write();
                    tracing::trace!(target: "frozen_diag", "enter: got VIEW_MODELS write lock");
                    let cur_focus = *focus_state.read();
                    let Some(idx) = cur_focus else {
                        return EventResult::Consumed;
                    };
                    if idx >= snapshot.items.len() {
                        // 快照缩短（reset/rewind）——焦点失效，退出导航
                        *focus_state.write() = None;
                        *FOCUSED_ENTRY_KEY.state().write() = None;
                        return EventResult::Consumed;
                    }
                    // [Slice 4 §6.8] 焦点在 pending interaction block 上：
                    // Enter 提交当前 option（双轨：响应 channel + 关闭模态层；
                    // InteractionResolved 由消费者发出）；Space 消费但不动作
                    // （防止泄漏给输入区插入空格）。提交后退出 entry 焦点。
                    if let Some(block) = pending_interaction_of(&snapshot.items[idx]) {
                        if !next_is_preview {
                            let opt = *option_state.read();
                            submit_interaction_option(block, opt);
                        }
                        *focus_state.write() = None;
                        *FOCUSED_ENTRY_KEY.state().write() = None;
                        tracing::trace!(target: "frozen_diag", "enter: interaction submit exit");
                        return EventResult::Consumed;
                    }
                    let result = apply_fold_toggle(&mut snapshot, idx, next_is_preview);
                    tracing::trace!(target: "frozen_diag", "enter: handler exit");
                    result
                }
                _ => EventResult::Ignored,
            }
        });
    }

    let prev_total_visual_rows = hooks.use_state(|| 0usize);
    // [Fix] resize 高度变化哨兵：vis_height 加入 effect 依赖，终端高度变化时触发
    // auto_follow 的 resize 跟随逻辑（否则 resize 缩小视口后底部 footer/spinner 消失）。
    let prev_vis_height = hooks.use_state(|| 0u16);
    // [Fix] Submit 强制滚底 / History 切换强制滚底：订阅 atom 重新渲染
    let _loading_epoch_atom = hooks.use_atom(&LOADING_EPOCH);
    let _reset_counter_atom = hooks.use_atom(&BRIDGE_RESET_COUNTER);
    let loading_epoch = LOADING_EPOCH.get();
    let prev_loading_epoch = hooks.use_state(|| loading_epoch);
    let bridge_reset_counter = BRIDGE_RESET_COUNTER.get();
    let prev_reset_counter = hooks.use_state(|| bridge_reset_counter);
    hooks.use_effect(
        {
            move || {
                scroll::run_auto_follow(&scroll::AutoFollowCtx {
                    total_visual_rows,
                    vis_height,
                    scroll_state,
                    prev_items_len,
                    last_scrolled_at,
                    items_len,
                    is_loading,
                    follow_bottom,
                    prev_total_visual_rows,
                    prev_vis_height,
                    loading_epoch,
                    prev_loading_epoch,
                    bridge_reset_counter,
                    prev_reset_counter,
                    // [Slice 4 §6.8] pending interaction block 锚定范围
                    // （每帧派生值；effect 依赖不含它——block 完成时 generation
                    // 变化触发 effect，用当帧最新值）。
                    anchor_visual_range,
                })
            }
        },
        (
            items_len,
            vm_generation,
            is_loading,
            total_visual_rows,
            vis_height,
        ),
    );

    if empty {
        // 重置滚动条字段，避免 Welcome 页面残留旧会话的滚动条
        *scrollbar_fields.write_no_update() = ScrollbarFields::default();
        // 清空 md 复制按钮映射——Welcome 页面无消息，防止点击残留按钮复制已消失内容
        *copy_buttons.write_no_update() = Arc::new(Vec::new());
        if let Some(lines) = brewed_lines {
            return element!(
                View(
                    flex_direction: Direction::Vertical,
                    width: Constraint::Fill(1),
                    height: Constraint::Fill(1),
                ) {
                    View(height: Constraint::Fill(1)) {
                        Welcome(width: grid.content_width())
                    }
                    Text(text: Paragraph::new(RatText::from(lines)).wrap(Wrap { trim: false }))
                }
            )
            .into_any();
        }
        return element!(
            View(width: Constraint::Fill(1), height: Constraint::Fill(1)) {
                Welcome(width: grid.content_width())
            }
        )
        .into_any();
    }

    // ── 视口裁剪渲染（移除 ScrollView 的全量大 buffer）──
    // [TRAP] 原 ScrollView + Paragraph-with-Wrap 组合是 100% CPU 的真凶：ScrollView 创建
    // (width × total_visual_rows) 大 buffer，Paragraph 渲染所有 N 行到这个 buffer 是
    // O(N×W) per render。Drag 60-120Hz × O(N×W) 直接拉满 CPU。
    //
    // 视口裁剪：只 clone + highlight + 渲染视口内 ~60 行（vis_height）。
    //   1. 通过 wrap_map_cache 二分查找视口对应的逻辑行 [vp_start, vp_end]
    //   2. vp_first_offset = scroll_y - wrap_map[vp_start].visual_start（首行视觉偏移）
    //   3. viewport_lines = clone(core[vp_start..=vp_end]) + 必要时附加 footer_lines
    //   4. Paragraph::scroll((vp_first_offset, 0)) 精确偏移首行
    //
    // highlight：视口内选区行用 sel_bg 背景。不再缓存 highlight 结果——视口裁剪后
    // 只有 ~60 行，highlight 成本可忽略。Drag 期间频繁跨逻辑行变化也不会卡。
    //
    // [DEBUG] PERI_NO_HIGHLIGHT=1 紧急回退——完全不进入 highlight 路径。
    let no_highlight = std::env::var("PERI_NO_HIGHLIGHT")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    // clamp scroll_y 不超过 max_scroll（替代 ScrollView 渲染时的 clamp）
    let max_scroll = total_visual_rows.saturating_sub(vis_height as usize);
    let scroll_y_raw = scroll_state.read().offset();
    let scroll_y = scroll_y_raw.min(max_scroll);

    // [Fix] 每帧钳制 scroll_state.offset 到 [0, max_scroll]。
    // apply_scroll 的 scroll_down() 无限递增 offset，没有上限感知——用户可以一直
    // 往下滚直到 offset 远超 max_scroll。虽然 scroll_y = raw.min(max_scroll) 让
    // 渲染正确，但 scroll_state 内部 offset 不被重置，往上滚时需要把多余 offset
    // 消耗完（如 offset=100, max_scroll=40 → 需滚 60 次才恢复）。
    // write_no_update 不触发 re-render，避免自激回路。
    if scroll_y_raw > max_scroll {
        scroll_state.write_no_update().set_offset(max_scroll);
    }

    // 更新 scrollbar fields——post_component_draw 时基于此渲染滚动条
    //
    // [Fix] ratatui Scrollbar 的 position 模型是 "item index"，max_position =
    // content_length - 1。但我们的 scroll_y 是 scroll offset，max = content_length -
    // vis_height。直接传 scroll_y 会导致 thumb 永远到不了底部（因为 scroll_y <
    // content_length - 1）。需要把 [0, max_scroll] 线性映射到 [0, content_length-1]。
    {
        let mut g = scrollbar_fields.write_no_update();
        g.content_length = total_visual_rows;
        g.position = scroll_y
            .saturating_mul(total_visual_rows.saturating_sub(1))
            .checked_div(max_scroll)
            .unwrap_or(0);
        g.viewport_length = vis_height as usize;
    }

    // [Fix 滚动尾随] 渲染帧兜底 flush：ratatui-kit 无 tick 定时器，停手后事件不再
    // 到达时，残留 pending_delta 在任意后续渲染帧（鼠标移动/内容更新/其他事件）
    // 落地，消除「滚动停止不到位」；反向抵消已由 apply_scroll 先行处理。
    // write_no_update 不触发 wake，无自激重渲染。
    scroll::flush_scroll_if_due(
        &scroll_throttle,
        &scroll_state,
        &scrollbar_fields,
        &follow_bottom,
    );

    let vp_height = vis_height as usize;

    // ── keepgoing 按钮屏幕位置（每帧更新）──
    // 必须在 scroll_y 计算之后、handler 使用之前；write_no_update 避免自激重渲染。
    {
        let mut k_rect = keepgoing_rect.write_no_update();
        *k_rect = compute_keepgoing_rect(
            empty,
            area_rect,
            keepgoing_layout,
            core_total_visual_rows,
            scroll_y,
            vis_height,
        );
    }

    // ── md 复制按钮屏幕位置（每帧更新）──
    // 按钮行在 slot 内是唯一视觉行（copy_button_line 已保证不折行），
    // 屏幕行 = area.y + slot 视觉偏移 + 行内视觉偏移 - scroll_y。
    // 视口外的按钮不进入映射（点不到），避免列表随会话增长。
    {
        let mut hits: Vec<CopyButtonHit> = Vec::new();
        if let Some(area) = area_rect {
            let vp_end = area.y.saturating_add(vis_height);
            let caches_read = vm_caches.read();
            // [LOW-5] 点击校验的身份 hash：运行中 bubble 的 content_hash 每秒
            // 随 duration 漂移（G1 按秒刷新时长文本），跨秒点击会偶发拒绝——
            // 命中映射保存稳定身份 hash（assistant 排除时变 duration），
            // 事件时点按同口径比对。快照索引可能落后于 vm_caches（TOCTOU
            // 防御同 rebuild 阶段：越界回退 slot.content_hash）。
            let vms_guard = view_models.read();
            for (slot_index, slot) in caches_read.iter().enumerate() {
                let Some(btn) = &slot.copy_button else {
                    continue;
                };
                let Some(entry) = slot.wrap_map.get(btn.logical_idx) else {
                    continue;
                };
                let vis_row = slot_visual_starts
                    .get(slot_index)
                    .copied()
                    .unwrap_or(0)
                    .saturating_add(entry.visual_start);
                let row = area.y as i64 + vis_row as i64 - scroll_y as i64;
                if row < area.y as i64 || row >= vp_end as i64 {
                    continue;
                }
                let hit_hash = match vms_guard.items.get(slot_index) {
                    Some(TuiRenderUnit::TuiAssistantBubble(b)) => {
                        TuiAssistantBubble::stable_identity_hash(&b.text, b.reasoning.as_ref())
                    }
                    _ => slot.content_hash,
                };
                hits.push(CopyButtonHit {
                    row: row as u16,
                    x_start: area.x.saturating_add(btn.x_start),
                    x_end: area.x.saturating_add(btn.x_end),
                    slot_index,
                    vm_hash: hit_hash,
                });
            }
        }
        *copy_buttons.write_no_update() = Arc::new(hits);
    }

    // ── [Slice 4 §6.8] interaction option 屏幕位置 + 当前项高亮信息（每帧更新）──
    // 与 md 复制按钮同模式：视口外的选项不进映射（点不到）；高亮只作用于
    // 视口行（G3 视口级，不动渲染缓存）。返回值供视口循环对「焦点 slot 的
    // 当前 option 行」应用 selection bg + bold（§9）。
    let interaction_highlight: Option<(usize, usize)> = {
        let mut hits: Vec<InteractionOptionHit> = Vec::new();
        let mut focused_interaction_row: Option<(usize, usize)> = None;
        let focus_slot = *entry_focus.read();
        let cur_option = *interaction_option.read();
        if let Some(area) = area_rect {
            let vp_end = area.y.saturating_add(vis_height);
            let caches_read = vm_caches.read();
            for (slot_index, slot) in caches_read.iter().enumerate() {
                let Some(il) = &slot.interaction else {
                    continue;
                };
                for (opt_i, row_local) in il.option_rows.iter().enumerate() {
                    let Some(entry) = slot.wrap_map.get(*row_local) else {
                        continue;
                    };
                    let vis_row = slot_visual_starts
                        .get(slot_index)
                        .copied()
                        .unwrap_or(0)
                        .saturating_add(entry.visual_start);
                    let row = area.y as i64 + vis_row as i64 - scroll_y as i64;
                    if row < area.y as i64 || row >= vp_end as i64 {
                        continue;
                    }
                    // 列区间：横向布局按 option 文本区间；垂直/超宽整行命中
                    let (x_start, x_end) = match il.option_cols.get(opt_i).copied().flatten() {
                        Some((s, e)) => (area.x.saturating_add(s), area.x.saturating_add(e)),
                        None => (
                            area.x,
                            area.x
                                .saturating_add(area.width)
                                .max(area.x.saturating_add(1)),
                        ),
                    };
                    hits.push(InteractionOptionHit {
                        row: row as u16,
                        x_start,
                        x_end,
                        slot_index,
                        vm_hash: slot.content_hash,
                        option_index: opt_i,
                    });
                    if focus_slot == Some(slot_index) && opt_i == cur_option {
                        focused_interaction_row = Some((slot_index, *row_local));
                    }
                }
            }
        }
        *interaction_rects.write_no_update() = Arc::new(hits);
        focused_interaction_row
    };

    // core_total_visual_rows 在前面拼接 wrap_map 时算出，直接复用。

    // 选区范围（字符级）：(first_logical, last_logical, sr, sc, er, ec)
    // 视口外选区不参与 highlight，selection state 保留供复制
    // [Why] 字符级高亮——旧版只存 (first_logical, last_logical) 整逻辑行范围，
    // 导致整行背景色覆盖；与字符级复制提取不一致。现在保留完整 (sr, sc, er, ec)，
    // highlight_line_in_selection 用 wrap_byte_starts 算 byte 范围，拆分 spans。
    // sr/er 为视觉行（usize，内容可超 65535 视觉行），sc/ec 为视觉列（u16）。
    let sel_bounds: Option<(usize, usize, usize, u16, usize, u16)> = if !no_highlight {
        let sel = text_sel.read();
        if let Some(((sr, sc), (er, ec))) = sel.normalized_bounds() {
            // Clamp sr/er 到 wrap_map 视觉范围内（footer 区域无 wrap_map）
            let max_visual = concat_wrap_map_arc
                .last()
                .map(|e| e.visual_end.saturating_sub(1))
                .unwrap_or(0);
            let sr_c = sr.min(max_visual);
            let er_c = er.min(max_visual);
            match (
                visual_to_logical(sr_c, &concat_wrap_map_arc),
                visual_to_logical(er_c, &concat_wrap_map_arc),
            ) {
                (Some(f), Some(l)) => Some((f.min(l), f.max(l), sr_c, sc, er_c, ec)),
                _ => None,
            }
        } else {
            None
        }
    } else {
        None
    };

    // 视口对应的 core 逻辑行范围 + 首行视觉偏移
    let (vp_core_start, vp_core_end, vp_first_offset): (usize, usize, u16) =
        if scroll_y < core_total_visual_rows && total_logical_lines > 0 {
            viewport_logical_range(&concat_wrap_map_arc, scroll_y, vp_height).unwrap_or((0, 0, 0))
        } else {
            // 视口完全在 footer 内（footer 占据末尾几行）
            (0, 0, 0)
        };

    // 视口是否包含 footer（视口末尾超出 core 总视觉行数）
    let viewport_has_footer =
        !empty && !footer_lines.is_empty() && scroll_y + vp_height > core_total_visual_rows;

    // 构建 viewport_lines：clone + highlight 视口内的 core 行，必要时附加 footer
    let sel_bg = THEME_ATOM.state().read().semantic.surface.selection;
    // [Slice 3] §9 focus 视觉（G3：只作用于视口行，不写缓存/业务状态）：
    // focused entry 左缘 selection border（outer 列，用 ▌ 字形——NO_COLOR 剥离后仍可见）。
    let focus_slot = *entry_focus.read();
    let focus_border_style = Style::default().fg(sel_bg).add_modifier(Modifier::BOLD);
    // [F3 §12] 焦点 border 字形走符号降级表（unicode 不足时 ▌ → |，
    // 不输出原始 UTF-8 缺字盒）。
    let focus_border_glyph =
        crate::kit::terminal_caps::symbols(&crate::kit::atoms::TERMINAL_CAPS.state().read())
            .focus_border;
    let core_len = total_logical_lines;
    let mut viewport_lines: Vec<Line<'static>> = Vec::with_capacity(
        (vp_core_end.saturating_sub(vp_core_start) + 1)
            .min(vp_height + 2)
            .saturating_add(footer_lines.len()),
    );

    if scroll_y < core_total_visual_rows && vp_core_start <= vp_core_end && core_len > 0 {
        let end = vp_core_end.min(core_len - 1);
        // concat_wrap_map_arc 已是 Arc，clone 引用计数 O(1)
        let wrap_map_arc = Arc::clone(&concat_wrap_map_arc);
        // [Scheme D] 通过 slot_index + slot_offsets 按需从 slot 中提取行
        let slots_arc = Arc::clone(&slot_arcs_arc);
        let offsets_arc = Arc::clone(&slot_offsets_arc);
        for i in vp_core_start..=end {
            let in_sel = sel_bounds.is_some_and(|(f, l, _, _, _, _)| i >= f && i <= l);
            if let Some(entry) = wrap_map_arc.get(i) {
                let local_idx = i - offsets_arc[entry.slot_index];
                let line = &slots_arc[entry.slot_index][local_idx];
                let mut out = if in_sel {
                    let (_, _, sr, sc, er, ec) = sel_bounds.unwrap();
                    highlight_line_in_selection(line, entry, sr, er, sc, ec, vis_width, sel_bg)
                } else {
                    line.clone()
                };
                // [Slice 3] focus 视觉 post-pass——替换首列 outer 空 cell
                // （渲染层约定：非空行首 span 恒为裸空格 outer cell）。
                if focus_slot == Some(entry.slot_index)
                    && let Some(first) = out.spans.first_mut()
                    && first.content.as_ref() == " "
                    && first.style == Style::default()
                {
                    *first = Span::styled(focus_border_glyph, focus_border_style);
                }
                // [Slice 4 §6.8] interaction「当前项」高亮：焦点 slot 的当前
                // option 行（§9：selection bg + border + bold——border 是选项
                // 静态 `[ ]` 外框，bg + bold 在此应用）。
                if interaction_highlight == Some((entry.slot_index, local_idx))
                    && !out.spans.is_empty()
                {
                    out.style = out.style.bg(sel_bg).add_modifier(Modifier::BOLD);
                }
                viewport_lines.push(out);
            }
        }
    }

    // [Slice 2] §8.1 `↓ New output` 指示器——浏览态（用户滚离底部）且视口未到
    // 真实内容底时，在视口末尾（有 footer 时插在 footer 之前）插入指示行。
    // 视口附加行：不进 VmCacheSlot / wrap_map / total_visual_rows（G3 视口级）；
    // NO_COLOR 剥离 pass（下方）天然覆盖（文本保留、颜色剥离）。
    // [Why 内容底口径] total_visual_rows 含 SCROLL_PADDING 缓冲（不可见行），
    // 判定以「真实内容底」= core + footer 视觉行数为准——滚到视觉底部即消失，
    // 与粘性 follow 恢复（should_follow_after_user_scroll 扣缓冲）口径对齐。
    let new_output_active = scroll::new_output_indicator_active(
        *follow_bottom.read(),
        scroll_y,
        vp_height,
        core_total_visual_rows + footer_visual_rows,
    );
    {
        let mut rect = new_output_rect.write_no_update();
        if new_output_active && let Some(area) = area_rect {
            let arrow = if caps.read().unicode { "\u{2193}" } else { "v" };
            let sem = THEME_ATOM.state().read().semantic;
            let mut spans = vec![Span::raw(" ".repeat(grid.first_prefix_width()))];
            spans.push(Span::styled(
                format!("{arrow} {}", crate::i18n::tr("msg-new-output")),
                Style::default()
                    .fg(sem.status.running)
                    .add_modifier(Modifier::BOLD),
            ));
            viewport_lines.push(Line::from(spans));
            // 屏幕行 = area.y + 指示行在 viewport_lines 中的索引 - vp_first_offset
            // （Paragraph::scroll 仅跳过首行的视觉偏移，viewport_lines 完整传入；
            // 指示行是 push 后的末元素）。
            let screen_row =
                area.y as i64 + viewport_lines.len() as i64 - 1 - vp_first_offset as i64;
            let vp_end = area.y as i64 + i64::from(vis_height);
            *rect = if screen_row >= area.y as i64 && screen_row < vp_end {
                let x_end = area
                    .x
                    .saturating_add(area.width)
                    .max(area.x.saturating_add(1));
                Some((screen_row as u16, area.x, x_end))
            } else {
                None
            };
        } else {
            *rect = None;
        }
    }

    if viewport_has_footer {
        viewport_lines.extend(footer_lines.iter().cloned());
    }
    // [NO_COLOR 剥离 pass]（§12）：只作用于视口内可见行，不写缓存/业务状态（G3）。
    // 剥离颜色但保留 modifier（bold/italic/dim）与符号、文本——状态不依赖颜色。
    if strip_color {
        for line in viewport_lines.iter_mut() {
            *line = strip_line_colors(line);
        }
    }
    // [PERI_RENDER_TIMING] 视口裁剪耗时
    let t_viewport = Instant::now();
    trace_phase(
        "viewport",
        t_concat,
        Some(&format!("vp_lines={}", viewport_lines.len())),
    );

    // Paragraph::scroll 偏移：core 内的偏移 = vp_first_offset
    // 视口完全在 footer 内时（scroll_y >= core_total_visual_rows），按 footer 内偏移
    let scroll_offset_y: u16 = if scroll_y >= core_total_visual_rows && core_total_visual_rows > 0 {
        (scroll_y - core_total_visual_rows) as u16
    } else {
        vp_first_offset
    };

    // [Why] View 必须保持 `Fill(1)`——ScrollbarHook 在 MessageArea 的 drawer.area
    // 最右 1 列渲染滚动条 thumb。若 View 改为 `Max(vis_width)`，View 自身 area 缩到
    // vis_width，ScrollbarHook 的 drawer.area 也跟着缩，导致 thumb 渲染在 area.width-2
    // 处（向左偏 1 列）。
    //
    // 让 Paragraph 实际 wrap 宽度 = vis_width 的正确做法：给 Paragraph 套
    // `Block::default().padding(Padding::new(0, 1, 0, 0))`（右 padding 1）。Block 占满
    // View 的 area.width，内部 wrap 宽度 = area.width - 1 = vis_width，与
    // `total_visual_rows` / `wrap_map_cache` / `line_count(vis_width)` 的估算一致；
    // 右 padding 1 列留给 scrollbar thumb（post_component_draw 时绘制覆盖 padding 空白）。
    // [PERI_RENDER_TIMING] 帧总耗时
    trace_phase(
        "frame-total",
        frame_t0.unwrap_or(t_viewport),
        Some(&format!("gen={vm_generation}")),
    );
    element!(
        View(
            flex_direction: Direction::Vertical,
            width: Constraint::Fill(1),
            height: Constraint::Fill(1),
        ) {
            Text(text: Paragraph::new(RatText::from(viewport_lines))
                .wrap(Wrap { trim: false })
                .block(Block::default().padding(Padding::new(0, 1, 0, 0)))
                .scroll((scroll_offset_y, 0)))
        }
    )
    .into_any()
}

/// 计算 keepgoing 按钮的屏幕点击区域 `(y, x_start, width)`。
///
/// 渲染布局：core 行（`core_total_visual_rows` 行）→ footer_lines（`line_index` 行）
/// → padding 行。footer_lines[line_index] 的屏幕 y = area.y + core_total_visual_rows
/// + line_index - scroll_y（i64 数学避免 u16 下溢）。
///
/// 返回 None 的情形：
/// - empty：Welcome 布局（footer 渲染在 Welcome 之下，行位置模型不同）——按钮可见但不可点击
/// - 按钮被滚出视口（y 不在 [area.y, area.y + vis_height) 内）
fn compute_keepgoing_rect(
    empty: bool,
    area_rect: Option<ratatui_kit::ratatui::layout::Rect>,
    layout: Option<KeepGoingLayout>,
    core_total_visual_rows: usize,
    scroll_y: usize,
    vis_height: u16,
) -> Option<(u16, u16, u16)> {
    let area = area_rect?;
    let layout = layout?;
    if empty {
        return None;
    }
    let row =
        area.y as i64 + core_total_visual_rows as i64 + layout.line_index as i64 - scroll_y as i64;
    let vp_end = area.y as i64 + vis_height as i64;
    if row < area.y as i64 || row >= vp_end {
        return None;
    }
    Some((
        row as u16,
        area.x.saturating_add(layout.start_col),
        layout.width,
    ))
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
