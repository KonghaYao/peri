//! 历史折叠投影：canonical 历史与 UI 覆盖分离，稳定历史不逐 chunk 再处理。
use super::{SessionPhase, TuiRenderUnit};
use crate::kit::tui_render_unit::{EntryStatus, FoldKey, FoldState, FoldTarget, fold_for_status};
use std::collections::HashMap;

#[derive(Default)]
pub(crate) struct FoldedHistory {
    source: Option<im::Vector<TuiRenderUnit>>,
    phase: Option<SessionPhase>,
    overrides: HashMap<FoldKey, FoldState>,
    folded: im::Vector<TuiRenderUnit>,
}

impl FoldedHistory {
    pub(super) fn project(
        &mut self,
        source: &im::Vector<TuiRenderUnit>,
        phase: SessionPhase,
        overrides: &HashMap<FoldKey, FoldState>,
    ) -> im::Vector<TuiRenderUnit> {
        // 持有源的共享节点：任何原地 set/update 都会 COW，从而改变结构身份。
        // im 的 inline 小向量没有共享指针；只有这个固定容量分支做值比较。
        let same_source = self
            .source
            .as_ref()
            .is_some_and(|old| old.ptr_eq(source) || (source.is_inline() && old == source));
        if !same_source || self.phase != Some(phase) || self.overrides != *overrides {
            #[cfg(test)]
            crate::kit::acp_bridge::observe_perf(
                crate::kit::acp_bridge::PerfCounter::HistoryFoldVisits,
                source.len() as u64,
            );
            self.folded = source.clone();
            apply_fold_pass(&mut self.folded, phase, overrides);
            self.source = Some(source.clone());
            self.phase = Some(phase);
            self.overrides = overrides.clone();
        }
        self.folded.clone()
    }
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
/// [G3] active projection 每次发布调用；committed 仅缓存失效时调用。
/// 单次 pass 为 O(N) 扫描，只对变化项克隆+set。
pub(super) fn apply_fold_pass(
    items: &mut im::Vector<TuiRenderUnit>,
    phase: SessionPhase,
    overrides: &HashMap<FoldKey, FoldState>,
) {
    use TuiRenderUnit::*;
    // [PERF] 读引用代替快照克隆——表只被键盘 handler 低频写入，pass 本身
    // 不写该表（迭代期只读 + 末尾 COW set，无嵌套锁获取），持读锁安全；
    // 空表短路：热路径（无手动覆盖）跳过全部查表与 FoldKey 构造克隆。
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
