//! [REQ-C01-3 / A20] 视口锚点身份重定位——渲染侧唯一的「阅读位置不跳动」判据。
//!
//! ## 为什么只能在渲染侧做
//!
//! 更早历史并入发生在桥接任务（`state.committed` 的槽位坐标），而屏幕上的槽位是
//! **发布后** `VIEW_MODELS.items` 的下标——发布前会跑折叠 pass、插入 turn divider
//! 与 todo 摘要，两套槽位坐标不可互换（S7d 复核 §5.1 判据 5 = FAIL 的那条路径）。
//! 因此本模块不接收任何桥接槽位号：锚点用**单元自身的稳定身份**表达，重定位在拥有
//! 发布后 `snapshot.items` 与 `SlotIndex` 的渲染侧完成。
//!
//! ## 语义
//!
//! 1. [`ViewportAnchor::capture`] 读取顶行单元的稳定身份和视觉行偏移。自动生成的
//!    turn divider 绑定紧随其后的用户气泡，保存顶行在该气泡起点之前的行数。
//! 2. [`ViewportAnchor::relocate`] 在新索引中定位同一单元。分隔行必须仍隶属该用户
//!    气泡前的 divider；普通正文保持原来在单元内的视觉行偏移。
//! 3. 应用点：`scroll::auto_follow::run_auto_follow` 的并入分支（**仅浏览态**；跟随态与
//!    非浏览态语义不变），由 [`crate::kit::message_area::viewport_anchor::PrependRelocation`]
//!    承载一次并入的换算结果，`Transcript::prepare` 产出、渲染 body 取走、effect 消费（一次性）。
//!
//! ## 身份缺失（安全方向）
//!
//! 新窗口里**认不出**锚点身份时一律**不补偿**（`None` → 偏移不动），并在
//! `msg_scroll_diag` 目标下留 trace 记录。认不出的两种来源：
//!
//! 1. 锚点单元被移除 / 被 rewind 替换（身份不在新 `items` 里）；
//! 2. 同一身份在新窗口出现**多次**（内容完全相同的单元，如两条一样的用户气泡）→
//!    歧义，不猜。
//!
//! 不补偿的代价是这一次并入后视口按内容插入量下移（A20 未覆盖），但绝不会把视口
//! 推到窗口底部（过补偿 = 判据 5 FAIL 时的实际后果）；两个方向都不理想时优先不误动。
//!
//! ## 仍未覆盖的边界（不是乐观表述）
//!
//! 1. **锚点粒度 = 顶层单元**：锚点是视口顶行所在的顶层渲染单元，不含嵌套子项
//!    （`TuiSubAgentGroup.view_models` 的内部单元）。
//!    `SlotIndex` 只给整槽的视觉区间，给不出嵌套子项的视觉起点——锚点落在展开的组内部
//!    时，组**位置**会被钉住，组内那一行不保证。
//! 2. **单元内位移越界**：锚点单元在新布局里变矮（内容被折叠 / 缩短）时，
//!    `target_offset` 可能落到该单元之外（视口顶行落到下一个单元里）。此时不夹取、
//!    不额外补偿——夹取会把位移量悄悄改掉，反而引入第二种语义。
//! 3. **同帧多次并入**：一帧内落了多页时，`prepare` 只按**最新**快照重定位一次
//!    （身份重定位对「插入了多少块」不敏感，所以结果仍然正确；被折叠的是中间的诊断粒度）。
//! 4. **并入代数和快照共同提交**：桥接持 `VIEW_MODELS` 写锁发布快照及其并入/释放代数，
//!    渲染持同一读锁取得两者。先到的本地折叠沿用上次已提交代数，等实际页面发布后再重定位。
//!
//! 移除条件：`SlotIndex` 能给出「单元内嵌套子项的视觉起点」后，边界 1 可以按子项身份
//! 细化；本模块的存在理由（跨层槽位不可互换）不会因此消失。

use crate::kit::tui_render_unit::{FoldKey, TuiAssistantBubble, TuiRenderUnit};

use super::selection::SlotIndex;
use super::vm_cache_slot::VmCacheSlot;
use crate::kit::history_window::WindowUnitToken;
use crate::kit::markdown::MermaidFenceKey;
use std::hash::{Hash, Hasher};

/// 单元身份：优先既有折叠键，其次既有内容身份。
///
/// 不新造身份体系——两类来源都是仓库里已经在用的身份事实（折叠键见
/// `FoldKey::of`，内容身份见 `hits.rs` 的点击陈旧校验）。
#[derive(Debug, Clone, PartialEq, Eq)]
enum UnitIdentity {
    /// 折叠键（`FoldKey`）：工具 `tool_id` / 子 agent 实例 / 交互 `request_id` /
    /// 系统提醒 id / reasoning `message_id`。
    Fold(FoldKey),
    UserMessage(String),
    /// 其余单元：既有内容身份 hash + 单元种类判别位。
    Content {
        kind: u8,
        hash: u64,
    },
}

impl UnitIdentity {
    /// 该身份是否就是这个单元。`kind` 先比、内容 hash 后算——种类不符时不做文本 hash。
    fn is(&self, vm: &TuiRenderUnit) -> bool {
        match self {
            Self::Fold(key) => FoldKey::of(vm).as_ref() == Some(key),
            Self::UserMessage(id) => {
                matches!(vm, TuiRenderUnit::TuiUserBubble(bubble) if bubble.message_id.as_ref() == Some(id))
            }
            Self::Content { kind, hash } => {
                content_kind(vm) == *kind && content_identity_hash(vm) == *hash
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AnchorRows {
    Inside(usize),
    BeforePrompt(usize),
}

fn is_turn_divider(unit: &TuiRenderUnit) -> bool {
    matches!(unit, TuiRenderUnit::TuiDivider(divider) if divider.label.is_none())
}

/// 视口顶部可见单元的锚点：身份 + 视口顶行在该单元内的位移。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ViewportAnchor {
    identity: UnitIdentity,
    slot: usize,
    rows: AnchorRows,
}

impl ViewportAnchor {
    /// 发布**前**的锚点：从当前（旧）布局里取视口顶行所在单元的身份与单元内位移。
    ///
    /// 取不到时返回 `None`（不补偿）：索引还没有内容（首帧）、`scroll_y` 越过 core 视觉
    /// 总行数（含跟随态 `usize::MAX` 哨兵、视口停在 SCROLL_PADDING / footer 段）、
    /// 或槽位与单元对不上（索引早于当帧快照）。
    pub(super) fn capture(index: &SlotIndex, scroll_y: usize) -> Option<Self> {
        let items = index.canonical_items()?;
        let slot = index.visual_lookup(scroll_y)?.slot_index;
        let vm = items.get(slot)?;
        if is_turn_divider(vm) {
            let prompt = slot + 1;
            let user @ TuiRenderUnit::TuiUserBubble(_) = items.get(prompt)? else {
                return None;
            };
            let distance = index.slot_visual_start(prompt)?.checked_sub(scroll_y)?;
            return Some(Self {
                identity: identity_of(user),
                slot: prompt,
                rows: AnchorRows::BeforePrompt(distance),
            });
        }
        Some(Self {
            identity: identity_of(vm),
            slot,
            rows: AnchorRows::Inside(scroll_y.checked_sub(index.slot_visual_start(slot)?)?),
        })
    }

    /// 发布**后**的目标视口偏移：按同一身份在新布局里重查视觉起点。
    ///
    /// 命中唯一槽位 → `Some(新起点 + 单元内位移)`；命中 0 个（身份消失）或 ≥2 个
    /// （身份歧义）→ `None`（不补偿，安全方向）。
    pub(super) fn relocate(&self, index: &SlotIndex) -> Option<usize> {
        let items = index.canonical_items()?;
        let mut hit: Option<usize> = None;
        {
            let iteration_items = items.iter().enumerate();
            for (slot, vm) in iteration_items {
                if !self.identity.is(vm) {
                    continue;
                }
                if hit.is_some() {
                    tracing::trace!(
                        target: "msg_scroll_diag",
                        "prepend anchor: identity is ambiguous in the new window → no compensation",
                    );
                    return None;
                }
                hit = Some(slot);
            }
        }
        let Some(slot) = hit else {
            tracing::trace!(
                target: "msg_scroll_diag",
                "prepend anchor: identity not found in the new window → no compensation",
            );
            return None;
        };
        self.target_offset(index, slot)
    }

    /// When the measured run survives, its old slot is an exact candidate in the new
    /// projection. Counts include duplicates outside the run (including newly added entries).
    pub(super) fn relocate_page(
        &self,
        index: &SlotIndex,
        run: crate::kit::tui_render_unit::HistoryOverlap,
        counts: &std::collections::HashMap<WindowUnitToken, usize>,
        caches: &[VmCacheSlot],
    ) -> Option<usize> {
        let token = self.window_token();
        if counts.get(&token) != Some(&1) {
            return None;
        }
        let slot = if (run.old_start..run.old_start + run.len).contains(&self.slot) {
            run.new_start + (self.slot - run.old_start)
        } else {
            caches
                .iter()
                .position(|cache| cache.anchor_token == Some(token))?
        };
        self.identity
            .is(index.canonical_items()?.get(slot)?)
            .then_some(())?;
        self.target_offset(index, slot)
    }

    fn target_offset(&self, index: &SlotIndex, slot: usize) -> Option<usize> {
        let start = index.slot_visual_start(slot)?;
        match self.rows {
            AnchorRows::Inside(rows) => Some(start.saturating_add(rows)),
            AnchorRows::BeforePrompt(rows) => {
                let divider = slot.checked_sub(1)?;
                let target = start.checked_sub(rows)?;
                let (first, end) = index.slot_visual_range(divider)?;
                (is_turn_divider(index.canonical_items()?.get(divider)?)
                    && (first..end).contains(&target))
                .then_some(target)
            }
        }
    }

    pub(super) fn window_token(&self) -> WindowUnitToken {
        identity_token(&self.identity)
    }

    #[must_use]
    pub(super) fn offset_within_unit(&self) -> usize {
        match self.rows {
            AnchorRows::Inside(rows) => rows,
            AnchorRows::BeforePrompt(_) => 0,
        }
    }
}

/// 顶行落在围栏内、邻接正文或普通行时的源位置。
#[derive(Debug, Clone, PartialEq, Eq)]
enum ReadingPosition {
    Fence {
        key: MermaidFenceKey,
        /// 围栏内逻辑行偏移。
        offset: usize,
        /// 折行内的视觉行偏移。
        row: usize,
    },
    AfterFence {
        key: MermaidFenceKey,
        /// 顶行在围栏之后第几行（0 = 围栏后第一行）。
        distance: usize,
        row: usize,
    },
    BeforeFence {
        key: MermaidFenceKey,
        /// 顶行在围栏之前第几行（1 = 紧邻围栏上一行）。
        distance: usize,
        row: usize,
    },
    Line {
        /// 无围栏消息里的逻辑行号。
        index: usize,
        row: usize,
    },
}

/// 同一单元内的阅读位置锚点：图完成、缩放或异步结果改变高度后，把顶行
/// 按围栏/相邻正文关系找回同一内容。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ReadingAnchor {
    identity: UnitIdentity,
    position: ReadingPosition,
}

impl ReadingAnchor {
    /// 布局重建**前**捕获（旧索引 + 旧缓存仍在位）。
    pub(super) fn capture(
        index: &SlotIndex,
        scroll_y: usize,
        caches: &[VmCacheSlot],
    ) -> Option<Self> {
        let items = index.canonical_items()?;
        let lookup = index.visual_lookup(scroll_y)?;
        let vm = items.get(lookup.slot_index)?;
        let cache = caches.get(lookup.slot_index)?;
        if cache.wrap_map.is_empty() {
            return None;
        }
        let row = scroll_y.saturating_sub(lookup.global_visual_start);
        let local = lookup.local_logical;
        let fences = &cache.mermaid_fences;
        let position = fences
            .iter()
            .find_map(|span| {
                if span.logical.contains(&local) {
                    Some(ReadingPosition::Fence {
                        key: span.key.clone(),
                        offset: local.saturating_sub(span.logical.start),
                        row,
                    })
                } else if span.logical.start > local {
                    Some(ReadingPosition::BeforeFence {
                        key: span.key.clone(),
                        distance: span.logical.start - local,
                        row,
                    })
                } else {
                    None
                }
            })
            .or_else(|| {
                fences
                    .iter()
                    .rev()
                    .find(|span| span.logical.end <= local)
                    .map(|span| ReadingPosition::AfterFence {
                        key: span.key.clone(),
                        distance: local.saturating_sub(span.logical.end),
                        row,
                    })
            })
            .unwrap_or(ReadingPosition::Line { index: local, row });
        Some(Self {
            identity: identity_of(vm),
            position,
        })
    }

    /// 布局重建**后**按身份与围栏关系找回目标偏移。
    pub(super) fn relocate(&self, index: &SlotIndex, caches: &[VmCacheSlot]) -> Option<usize> {
        let slot = if let Some(key) = self.position.fence_key() {
            // 围栏身份已含消息 + 原文位置，可直接定位到唯一槽位。
            caches
                .iter()
                .position(|cache| cache.mermaid_fences.iter().any(|span| span.key == *key))?
        } else {
            let items = index.canonical_items()?;
            let mut hit = None;
            for (slot, vm) in items.iter().enumerate() {
                if !self.identity.is(vm) {
                    continue;
                }
                if hit.is_some() {
                    return None;
                }
                hit = Some(slot);
            }
            hit?
        };
        let start = index.slot_visual_start(slot)?;
        let cache = caches.get(slot)?;
        let logical = self.position.resolve(cache)?;
        let line = cache.wrap_map.get(logical)?;
        let row = self.position.row();
        let height = line.visual_end.saturating_sub(line.visual_start);
        Some(
            start
                .saturating_add(line.visual_start)
                .saturating_add(row.min(height.saturating_sub(1))),
        )
    }
}

impl ReadingPosition {
    fn fence_key(&self) -> Option<&MermaidFenceKey> {
        match self {
            Self::Fence { key, .. }
            | Self::AfterFence { key, .. }
            | Self::BeforeFence { key, .. } => Some(key),
            Self::Line { .. } => None,
        }
    }

    fn row(&self) -> usize {
        match self {
            Self::Fence { row, .. }
            | Self::AfterFence { row, .. }
            | Self::BeforeFence { row, .. }
            | Self::Line { row, .. } => *row,
        }
    }

    fn resolve(&self, cache: &VmCacheSlot) -> Option<usize> {
        let lines = cache.wrap_map.len();
        if lines == 0 {
            return None;
        }
        let clamp = |value: usize| value.min(lines.saturating_sub(1));
        match self {
            Self::Fence { key, offset, .. } => {
                let span = cache.mermaid_fences.iter().find(|span| span.key == *key)?;
                let length = span.logical.end.saturating_sub(span.logical.start);
                Some(clamp(
                    span.logical
                        .start
                        .saturating_add((*offset).min(length.saturating_sub(1))),
                ))
            }
            Self::AfterFence { key, distance, .. } => {
                let span = cache.mermaid_fences.iter().find(|span| span.key == *key)?;
                Some(clamp(span.logical.end.saturating_add(*distance)))
            }
            Self::BeforeFence { key, distance, .. } => {
                let span = cache.mermaid_fences.iter().find(|span| span.key == *key)?;
                Some(clamp(span.logical.start.saturating_sub(*distance)))
            }
            Self::Line { index, .. } => Some(clamp(*index)),
        }
    }
}

/// 一次并入的重定位结果（渲染侧派生，effect 消费；一次性值）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PrependRelocation {
    /// 记录锚点时的视口偏移（= 发布前的 `scroll_y`）。诊断 / 测试 / 相对量换算用。
    pub(super) from_offset: usize,
    /// 锚点身份在新布局里的位置 → 目标视口偏移（绝对值）。
    pub(super) target_offset: usize,
}

impl PrependRelocation {
    /// 并入帧要落到的新 offset：相对量口径（`scroll_y + (target - from)`），
    /// 避免用旧绝对值覆盖期间发生的其它滚动；跟随态与非浏览态返回 `None`。
    pub(super) fn target_for(&self, follow_bottom: bool, scroll_y: usize) -> Option<usize> {
        if follow_bottom || scroll_y == usize::MAX {
            return None;
        }
        let delta = self.target_offset as i64 - self.from_offset as i64;
        Some((scroll_y as i64 + delta).max(0) as usize)
    }
}

fn identity_of(vm: &TuiRenderUnit) -> UnitIdentity {
    if let TuiRenderUnit::TuiUserBubble(bubble) = vm
        && let Some(id) = &bubble.message_id
    {
        return UnitIdentity::UserMessage(id.clone());
    }
    match FoldKey::of(vm) {
        Some(key) => UnitIdentity::Fold(key),
        None => UnitIdentity::Content {
            kind: content_kind(vm),
            hash: content_identity_hash(vm),
        },
    }
}

/// 渲染单元的稳定 token；桥接侧与视口侧共用同一口径。
pub(crate) fn window_identity_token(vm: &TuiRenderUnit) -> WindowUnitToken {
    if let TuiRenderUnit::TuiUserBubble(bubble) = vm
        && let Some(id) = &bubble.message_id
    {
        return hashed_token(11, id);
    }
    identity_token(&identity_of(vm))
}

fn identity_token(identity: &UnitIdentity) -> WindowUnitToken {
    match identity {
        UnitIdentity::Fold(key) => hashed_token(0, key),
        UnitIdentity::UserMessage(id) => hashed_token(11, id),
        UnitIdentity::Content { kind, hash } => WindowUnitToken {
            kind: *kind,
            hash: *hash,
        },
    }
}

fn hashed_token(kind: u8, value: &impl Hash) -> WindowUnitToken {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hasher);
    WindowUnitToken {
        kind,
        hash: hasher.finish(),
    }
}

/// 内容身份 hash：assistant 正文走既有 `stable_identity_hash`（排除随时长漂移的
/// 字段，与 `hits.rs` 的点击陈旧校验同口径），其余单元用既有 `content_hash`。
fn content_identity_hash(vm: &TuiRenderUnit) -> u64 {
    match vm {
        TuiRenderUnit::TuiAssistantBubble(bubble) => {
            TuiAssistantBubble::stable_identity_hash(&bubble.text, bubble.reasoning.as_ref())
        }
        other => other.content_hash(),
    }
}

/// 单元种类判别位——内容身份的一部分：不同种类的单元即使内容 hash 相同也不互相认领
/// （例如所有 `TuiDivider` 的 `content_hash` 都是 0）。
#[must_use]
fn content_kind(vm: &TuiRenderUnit) -> u8 {
    match vm {
        TuiRenderUnit::TuiUserBubble(_) => 1,
        TuiRenderUnit::TuiAssistantBubble(_) => 2,
        TuiRenderUnit::TuiSystemNote(_) => 3,
        TuiRenderUnit::TuiSubAgentGroup(_) => 4,
        TuiRenderUnit::TuiCollapsedGroup(_) => 5,
        TuiRenderUnit::TuiDivider(_) => 6,
        TuiRenderUnit::TuiAskUserBlock(_) => 7,
        TuiRenderUnit::TuiSystemReminder(_) => 8,
        TuiRenderUnit::TuiTodoSummary(_) => 9,
        TuiRenderUnit::TuiToolCard(_) => 10,
    }
}

#[cfg(test)]
#[path = "viewport_anchor_e2e_test.rs"]
mod e2e_tests;

#[cfg(test)]
#[path = "viewport_anchor_test.rs"]
mod tests;
