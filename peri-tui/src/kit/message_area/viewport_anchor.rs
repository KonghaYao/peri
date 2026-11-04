//! [REQ-C01-3 / A20] 视口锚点身份重定位——渲染侧唯一的「阅读位置不跳动」判据。
//!
//! ## 为什么只能在渲染侧做
//!
//! 更早历史并入发生在桥接任务（`state.committed` 的槽位坐标），而屏幕上的槽位是
//! **发布后** `VIEW_MODELS.items` 的下标——发布前会跑折叠 pass 与
//! `group_successful_tools`（相邻成功工具卡压成一张 `TuiCollapsedGroup`），
//! **槽位数会变少**，两套槽位坐标不可互换（S7d 复核 §5.1 判据 5 = FAIL 的那条路径）。
//! 因此本模块不接收任何桥接槽位号：锚点用**单元自身的稳定身份**表达，重定位在拥有
//! 发布后 `snapshot.items` 与 `SlotIndex` 的渲染侧完成。
//!
//! ## 语义
//!
//! 1. [`ViewportAnchor::capture`]（**发布前**，旧索引仍在位）：记下视口顶部那一行落在
//!    哪个单元、该单元的稳定身份（[`UnitIdentity`]）、以及这一行相对该单元视觉起点的
//!    位移 `offset_within_unit = scroll_y - 单元视觉起点`。
//! 2. [`ViewportAnchor::relocate`]（**发布后**，新索引重建完成）：按同一身份在新窗口里
//!    重查该单元的视觉起点，目标偏移 `target_offset = 新起点 + offset_within_unit`
//!    （等价的另一种写法是任务书里的 `新起点 - within_unit_offset`，两者只差符号约定：
//!    本实现的 `offset_within_unit ≥ 0` 表示「视口顶行在该单元内向下偏移了多少行」）。
//! 3. 应用点：`scroll::auto_follow::run_auto_follow` 的并入分支（**仅浏览态**；跟随态与
//!    非浏览态语义不变），由 [`crate::kit::message_area::viewport_anchor::PrependRelocation`]
//!    承载一次并入的换算结果，`Transcript::prepare` 产出、渲染 body 取走、effect 消费（一次性）。
//!
//! ## 身份缺失（安全方向）
//!
//! 新窗口里**认不出**锚点身份时一律**不补偿**（`None` → 偏移不动），并在
//! `msg_scroll_diag` 目标下留 trace 记录。认不出的三种来源：
//!
//! 1. 锚点单元被移除 / 被 rewind 替换（身份不在新 `items` 里）；
//! 2. 锚点单元自身被折叠 pass 或 `group_successful_tools` **并入新折叠组**而消失
//!    （组身份是组成员序列，与原单元身份不同）；
//! 3. 同一身份在新窗口出现**多次**（内容完全相同的单元，如两条一样的用户气泡）→
//!    歧义，不猜。
//!
//! 不补偿的代价是这一次并入后视口按内容插入量下移（A20 未覆盖），但绝不会把视口
//! 推到窗口底部（过补偿 = 判据 5 FAIL 时的实际后果）；两个方向都不理想时优先不误动。
//!
//! ## 仍未覆盖的边界（不是乐观表述）
//!
//! 1. **锚点粒度 = 顶层单元**：锚点是视口顶行所在的顶层渲染单元，不含嵌套子项
//!    （`TuiCollapsedGroup.view_models` / `TuiSubAgentGroup.view_models` 的内部单元）。
//!    `SlotIndex` 只给整槽的视觉区间，给不出嵌套子项的视觉起点——锚点落在展开的组内部
//!    时，组**位置**会被钉住，组内那一行不保证。
//! 2. **单元内位移越界**：锚点单元在新布局里变矮（内容被折叠 / 缩短）时，
//!    `target_offset` 可能落到该单元之外（视口顶行落到下一个单元里）。此时不夹取、
//!    不额外补偿——夹取会把位移量悄悄改掉，反而引入第二种语义。
//! 3. **同帧多次并入**：一帧内落了多页时，`prepare` 只按**最新**快照重定位一次
//!    （身份重定位对「插入了多少块」不敏感，所以结果仍然正确；被折叠的是中间的诊断粒度）。
//! 4. **跑在渲染 body 早段，快到足以「晚一帧」但不会漏配**：视口事实与快照由渲染 body
//!    早段读入后传入 `prepare`。桥接任务若恰好在**快照读之后、并合代数读之前**推进发布，
//!    本帧的 `published` 仍是旧发布 → 本帧不重定位；下一帧快照已是新发布、`merge_epoch_seen`
//!    还没被推进（它只在观测到发布变化时推进）→ 正常配对，代价只是晚一帧。
//!    反方向（代数读到新值、快照还是旧的）不需要处理：桥接侧**先推代数再发布**
//!    （`acp_events/history_page.rs` 的 `HISTORY_PREPEND_EPOCH.set` 在 `push_view_models`
//!    之前），所以「快照可见 = 代数可见」。**调用点必须保持「快照读早于代数读」**
//!    （`message_area/mod.rs`：`read_render_snapshot()` 在 `HISTORY_PREPEND_EPOCH.get()`
//!    之前）——这是本条不变量成立的唯一前提。
//! 5. **锚点行落在纯展示行（turn divider）上**：divider 的内容身份是常量（`content_hash`
//!    恒为 0、`label` 为 `None`），新窗口里出现 ≥2 枚即判歧义 → 不补偿。turn divider 投影
//!    （`acp_events::render` 的 `insert_turn_dividers`：第 2 条起每条 prompt 前一行）
//!    让这一形态在浏览态常见；修复方向是让 divider 行改按「其后真实单元 + 负偏移」派生
//!    锚点（`offset_within_unit` 需带符号，见边界 2 的越界口径）。
//!
//! 移除条件：`SlotIndex` 能给出「单元内嵌套子项的视觉起点」后，边界 1 可以按子项身份
//! 细化；本模块的存在理由（跨层槽位不可互换）不会因此消失。

use crate::kit::tui_render_unit::{FoldKey, TuiAssistantBubble, TuiRenderUnit};

use super::entry_nav::fold_key_of;
use super::selection::SlotIndex;
use std::hash::{Hash, Hasher};

/// 渲染单元的稳定身份 token（种类 + 内容/折叠键 hash）。
///
/// 供选区锚点与视口锚点共用：它不是历史窗口身份，也不写入任何历史协议字段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct UnitToken {
    pub kind: u8,
    pub hash: u64,
}

/// 单元身份：优先既有折叠键，其次既有内容身份。
///
/// 不新造身份体系——两类来源都是仓库里已经在用的身份事实（折叠键见
/// `message_area::entry_nav::fold_key_of`，内容身份见 `hits.rs` 的点击陈旧校验）。
#[derive(Debug, Clone, PartialEq, Eq)]
enum UnitIdentity {
    /// 折叠键（`FoldKey`）：工具 `tool_id` / 折叠组成员 tool_id 序列 / 子 agent 实例 /
    /// 交互 `request_id` / 系统提醒 id / reasoning `message_id`。
    Fold(FoldKey),
    /// 其余单元：既有内容身份 hash + 单元种类判别位。
    Content { kind: u8, hash: u64 },
}

impl UnitIdentity {
    /// 该身份是否就是这个单元。`kind` 先比、内容 hash 后算——种类不符时不做文本 hash。
    fn is(&self, vm: &TuiRenderUnit) -> bool {
        match self {
            Self::Fold(key) => fold_key_of(vm).map(|(key, _)| key).as_ref() == Some(key),
            Self::Content { kind, hash } => {
                content_kind(vm) == *kind && content_identity_hash(vm) == *hash
            }
        }
    }
}

/// 视口顶部可见单元的锚点：身份 + 视口顶行在该单元内的位移。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ViewportAnchor {
    identity: UnitIdentity,
    /// `scroll_y - 单元视觉起点`（≥ 0）。见模块文档第 2 条的符号约定。
    offset_within_unit: usize,
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
        let start = index.slot_visual_start(slot)?;
        Some(Self {
            identity: identity_of(vm),
            offset_within_unit: scroll_y.saturating_sub(start),
        })
    }

    /// 发布**后**的目标视口偏移：按同一身份在新布局里重查视觉起点。
    ///
    /// 命中唯一槽位 → `Some(新起点 + 单元内位移)`；命中 0 个（身份消失）或 ≥2 个
    /// （身份歧义）→ `None`（不补偿，安全方向）。
    pub(super) fn relocate(&self, index: &SlotIndex) -> Option<usize> {
        let items = index.canonical_items()?;
        let mut hit: Option<usize> = None;
        for (slot, vm) in items.iter().enumerate() {
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
        let Some(slot) = hit else {
            tracing::trace!(
                target: "msg_scroll_diag",
                "prepend anchor: identity not found in the new window → no compensation",
            );
            return None;
        };
        let start = index.slot_visual_start(slot)?;
        Some(start.saturating_add(self.offset_within_unit))
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
    match fold_key_of(vm) {
        Some((key, _)) => UnitIdentity::Fold(key),
        None => UnitIdentity::Content {
            kind: content_kind(vm),
            hash: content_identity_hash(vm),
        },
    }
}

/// 渲染单元的稳定 token；选区锚点与视口侧共用同一口径。
pub(crate) fn unit_identity_token(vm: &TuiRenderUnit) -> UnitToken {
    match identity_of(vm) {
        UnitIdentity::Fold(key) => {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            key.hash(&mut hasher);
            UnitToken {
                kind: 0,
                hash: hasher.finish(),
            }
        }
        UnitIdentity::Content { kind, hash } => UnitToken { kind, hash },
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
#[path = "viewport_anchor_test.rs"]
mod tests;
