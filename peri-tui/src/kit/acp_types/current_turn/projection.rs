use super::super::tool_card::build_tool_card;
use super::{CurrentTurn, TurnSegment};
use crate::kit::tui_render_unit::{
    EntryStatus, FoldTarget, TuiAssistantBubble, TuiReasoningBlock, TuiRenderUnit, TuiSystemNote,
    entry_status_code, fold_for_status, fold_state_code, tui_hash_combine,
};
use std::time::Instant;

impl CurrentTurn {
    /// Accessor: returns cached ViewModels.
    ///
    /// 缓存由 `sync_cache` 增量维护：流式变更只置 dirty 标记，
    /// `invalidate_cache`（如 acp_bridge 1s tick 刷新工具时长）置位后在下次
    /// 调用时重同步。返回的 `im::Vector` 可 O(1) 克隆共享。
    pub fn view_models(&mut self) -> &im::Vector<TuiRenderUnit> {
        if self.cache_dirty {
            self.sync_cache();
        }
        &self.cached_view_models
    }

    /// 构造 reasoning 块 + 内容哈希。
    ///
    /// `text_hash`/`reasoning_hash` 是文本/推理区域的滚动哈希（增量维护的 open
    /// 值或冻结段存储值），组合公式与 [`TuiAssistantBubble::compute_hash`] 完全
    /// 一致——保证增量路径与从零重建（recompute_hash）产出相同的 hash。
    ///
    /// `reasoning_running`：true = trailing 流式段（status=Running、fold=Preview、
    /// started_at=推理起点）；false = 冻结段（status=Completed、fold=Collapsed、
    /// duration_ms=flush 时刻冻结值）。折叠 pass 在 phase 离开 PromptRunning 时
    /// 把 trailing 段翻转成 Completed。bubble 的 `message_id` 由调用方直接赋值
    /// （身份字段，不进 hash）。
    ///
    /// 方案 1 混合形态：推理已结束而正文仍流式时，`reasoning_running=false`
    /// 且 `text_started_at=Some`——推理块 Completed + 正文 Running（`◐ Thinking…`
    /// 停止，正文继续增长）。冻结段两个标志恒为冻结态。
    ///
    /// `text_started_at`：trailing 段的本 bubble 正文开始时刻——running 时写入
    /// `TuiAssistantBubble.started_at`；冻结段传 None（正文时长由折叠 pass 在
    /// 翻转点冻结，镜像 reasoning 机制）。
    ///
    /// [§6.3] 空 reasoning：reasoning_running 且 reasoning 为空时仍产出空文本
    /// 的推理块（`◐ Thinking…` 占位行，不出现空白 block）；`!reasoning_running`
    /// 的空 reasoning 返回 `None`（冻结段无占位，避免历史噪音）。
    fn build_bubble_parts(
        reasoning: &str,
        text_hash: u64,
        reasoning_hash: u64,
        reasoning_running: bool,
        reasoning_started_at: Option<Instant>,
        reasoning_duration_ms: Option<u64>,
        text_started_at: Option<Instant>,
    ) -> (Option<TuiReasoningBlock>, u64) {
        let block = if reasoning.is_empty() && !reasoning_running {
            None
        } else {
            let status = if reasoning_running {
                EntryStatus::Running
            } else {
                EntryStatus::Completed
            };
            Some(TuiReasoningBlock {
                text: reasoning.to_string(),
                fold: fold_for_status(FoldTarget::Reasoning, status),
                status,
                is_running: reasoning_running,
                started_at: if reasoning_running {
                    reasoning_started_at
                } else {
                    None
                },
                duration_ms: if reasoning_running {
                    None
                } else {
                    reasoning_duration_ms
                },
            })
        };
        // [G1] 与 TuiAssistantBubble::compute_hash 同序同码——fold/status/is_running/
        // duration 逐项组合，末尾追加正文时长秒数（running 取 started_at 已耗时，
        // 冻结段取 duration_ms/1000，均秒取整；None→0）与冻结判别位
        // （`text_started_at.is_none()`，镜像 recompute_hash 的 started_at 口径），
        // 保证增量路径与 recompute_hash 产出相同 hash。
        let text_duration_secs = text_started_at.map(|t| t.elapsed().as_secs()).unwrap_or(0);
        let text_frozen = u64::from(text_started_at.is_none());
        let content_hash = match block.as_ref() {
            Some(r) => {
                let mut h = tui_hash_combine(
                    tui_hash_combine(text_hash, reasoning_hash),
                    fold_state_code(r.fold),
                );
                h = tui_hash_combine(h, entry_status_code(r.status));
                h = tui_hash_combine(h, u64::from(r.is_running));
                h = tui_hash_combine(h, r.duration_code());
                h = tui_hash_combine(h, text_duration_secs);
                tui_hash_combine(h, text_frozen)
            }
            None => {
                let h = tui_hash_combine(text_hash, text_duration_secs);
                tui_hash_combine(h, text_frozen)
            }
        };
        (block, content_hash)
    }

    /// 将 `cached_view_models` 与 `segments`/内容增量对齐——只重建/替换变化的
    /// 部分（trailing bubble、运行中或刚结束的工具卡、内容变化的 subagent 组），
    /// 冻结的 AssistantText 段与未变化的元素直接复用缓存。
    ///
    /// 每 token 成本 O(变化量 + 段数扫描)，取代旧版每 token 全量重建（O(总内容)
    /// 文本拷贝 + 全量 format!/hash 的 O(N²) 累积）。
    fn sync_cache(&mut self) {
        #[cfg(test)]
        {
            crate::kit::acp_bridge::observe_perf(
                crate::kit::acp_bridge::PerfCounter::Projection,
                1,
            );
            crate::kit::acp_bridge::observe_perf(
                crate::kit::acp_bridge::PerfCounter::ProjectionCopiedBytes,
                (self.text.len() + self.reasoning.len()) as u64,
            );
        }

        self.sync_segments();
        self.sync_trailing();

        // 后处理：将 Agent 工具卡片的 tool_calls_count 与紧随的 SubAgent 组配对。
        // 匹配逻辑：TuiToolCard(tool_name="Agent") 紧接着 TuiSubAgentGroup。
        self.pair_agent_tool_cards();

        self.cache_dirty = false;
    }

    /// 折叠归一化已删除（Slice 2）：折叠策略收敛到 `push_view_models` 的
    /// `apply_fold_pass` 单点（spec §7 表 + FOLD_OVERRIDES），缓存层不再内联
    /// 折叠决策——`sync_cache` 只负责按内容构建 VM，折叠由快照 pass 统一驱动。
    ///
    /// Agent 工具卡片与紧随的 SubAgent 组配对（tool_calls_count）。
    fn pair_agent_tool_cards(&mut self) {
        let mut updates: Vec<(usize, usize)> = Vec::new(); // (index, tool_count)
        let n = self.cached_view_models.len();
        for i in 0..n.saturating_sub(1) {
            if let (
                TuiRenderUnit::TuiToolCard(agent_card),
                TuiRenderUnit::TuiSubAgentGroup(subagent_group),
            ) = (&self.cached_view_models[i], &self.cached_view_models[i + 1])
                && agent_card.tool_name == "Agent"
                && agent_card.is_running
            {
                let tool_count = subagent_group
                    .view_models
                    .iter()
                    .filter(|vm| matches!(vm, TuiRenderUnit::TuiToolCard(_)))
                    .count();
                if tool_count > 0 && agent_card.tool_calls_count != tool_count {
                    updates.push((i, tool_count));
                }
            }
        }
        for (i, tool_count) in updates {
            if let TuiRenderUnit::TuiToolCard(card) = &self.cached_view_models[i] {
                let mut updated = card.clone();
                updated.tool_calls_count = tool_count;
                self.cached_view_models
                    .set(i, TuiRenderUnit::TuiToolCard(updated));
            }
        }
    }

    /// Patch chronological frozen segments, tools, and child projections.
    fn sync_segments(&mut self) {
        let mut prev_text_end: usize = 0;
        let mut prev_reasoning_end: usize = 0;

        for (i, segment) in self.segments.iter().enumerate() {
            match segment {
                TurnSegment::AssistantText {
                    text_end_byte,
                    reasoning_end_byte,
                    text_hash,
                    reasoning_hash,
                    message_id,
                    reasoning_duration_ms,
                } => {
                    let text_end = (*text_end_byte).min(self.text.len());
                    let reason_end = (*reasoning_end_byte).min(self.reasoning.len());
                    // 冻结段只构建一次，此后直接复用缓存（内容不再变化）。
                    if self.cached_view_models.len() <= i {
                        let text_slice = &self.text[prev_text_end..text_end];
                        let reasoning_slice = &self.reasoning[prev_reasoning_end..reason_end];
                        let (reasoning, content_hash) = Self::build_bubble_parts(
                            reasoning_slice,
                            *text_hash,
                            *reasoning_hash,
                            false,
                            None,
                            *reasoning_duration_ms,
                            None,
                        );
                        self.cached_view_models
                            .push_back(TuiRenderUnit::TuiAssistantBubble(TuiAssistantBubble {
                                text: text_slice.to_string(),
                                reasoning,
                                message_id: message_id.clone(),
                                // 冻结段无正文时长起点——时长由折叠 pass 在翻转点
                                // 对 trailing bubble 冻结；此处恒 None（G-Tokens）。
                                started_at: None,
                                duration_ms: None,
                                content_hash,
                            }));
                    }
                    prev_text_end = text_end;
                    prev_reasoning_end = reason_end;
                }
                TurnSegment::Tool { tool_idx } => {
                    if let Some(t) = self.tool_cards.get(*tool_idx) {
                        // 运行中卡片每 sync 重建（刷新 duration，hash 按秒变化）；
                        // 已结束卡片仅在 output 变化时重建一次。
                        let needs_rebuild = match self.cached_view_models.get(i) {
                            Some(TuiRenderUnit::TuiToolCard(c)) => {
                                c.is_running
                                    || Some(c.output_summary.as_str())
                                        != t.output_summary.as_deref()
                            }
                            _ => true,
                        };
                        if needs_rebuild {
                            let card = build_tool_card(t, self.active);
                            if self.cached_view_models.len() <= i {
                                self.cached_view_models
                                    .push_back(TuiRenderUnit::TuiToolCard(card));
                            } else {
                                self.cached_view_models
                                    .set(i, TuiRenderUnit::TuiToolCard(card));
                            }
                        }
                    }
                }
                TurnSegment::SubAgent { subagent_idx } => {
                    if let Some(s) = self.subagents.get_mut(*subagent_idx) {
                        let group_vm = s.view_model();
                        // O(1) hash 比较——未变化的 subagent 直接跳过 set()，
                        // 已变化的替换为新组（im::Vector set 走 COW，共享未变子节点）。
                        let changed = self
                            .cached_view_models
                            .get(i)
                            .is_none_or(|old| old.content_hash() != group_vm.content_hash());
                        if changed {
                            if self.cached_view_models.len() <= i {
                                self.cached_view_models.push_back(group_vm);
                            } else {
                                self.cached_view_models.set(i, group_vm);
                            }
                        }
                    }
                }
                TurnSegment::SystemNote {
                    text,
                    level,
                    content_hash,
                } => {
                    if self.cached_view_models.len() <= i {
                        self.cached_view_models
                            .push_back(TuiRenderUnit::TuiSystemNote(TuiSystemNote {
                                text: text.clone(),
                                level: level.clone(),
                                content_hash: *content_hash,
                            }));
                    }
                }
            }
        }
    }

    /// Rebuild only a growing or newly frozen trailing bubble.
    fn sync_trailing(&mut self) {
        // Trailing bubble（最后一个段之后仍未冻结的内容）——文本/推理增长时重建。
        // 长度比对是 O(1) 的变化检测：该区域 append-only，长度变 ⟺ 内容变。
        let has_trailing = self.text.len() > self.last_text_flush
            || self.reasoning.len() > self.last_reasoning_flush;
        if has_trailing {
            let trailing_idx = self.segments.len();
            let trailing_len_changed = match self.cached_view_models.get(trailing_idx) {
                Some(TuiRenderUnit::TuiAssistantBubble(b)) => {
                    b.text.len() != self.text.len() - self.last_text_flush
                        || b.reasoning.as_ref().map(|r| r.text.len()).unwrap_or(0)
                            != self.reasoning.len() - self.last_reasoning_flush
                }
                _ => true,
            };
            // [Fix §6.7] 冻结待消费（`freeze_trailing` 置位）：冻结只改 VM 形态
            // （started_at→None / duration_ms→Some / running→completed），不改
            // 文本长度——长度门控恒 false 会保留陈旧的 Running 形态 bubble
            // （详情面板对已完成 subagent 永久 `◐ Thinking… Ns`）。take() 消费
            // 后恢复长度门控（冻结重建恰一次，幂等）。
            let pending_freeze = self.trailing_frozen.is_some();
            if trailing_len_changed || pending_freeze {
                let text_slice = &self.text[self.last_text_flush..];
                let reasoning_slice = &self.reasoning[self.last_reasoning_flush..];
                // [§6.7] 冻结形态（stop_subagent 后）：Completed / Collapsed /
                // 冻结时长——`freeze_trailing` 已把 started_at 换算为 ms 并清除，
                // 此处直接构建（不经过顶层折叠 pass）。
                let trailing = if let Some((text_dur, reasoning_dur)) = self.trailing_frozen.take()
                {
                    let (reasoning, content_hash) = Self::build_bubble_parts(
                        reasoning_slice,
                        self.open_text_hash,
                        self.open_reasoning_hash,
                        false,
                        None,
                        reasoning_dur,
                        None,
                    );
                    let mut bubble = TuiAssistantBubble {
                        text: text_slice.to_string(),
                        reasoning,
                        message_id: self.last_message_id.clone(),
                        started_at: None,
                        duration_ms: text_dur,
                        content_hash,
                    };
                    // [G1] 单点重算：与折叠 pass 冻结后 recompute_hash 公式一致
                    // （build_bubble_parts 的 !running 路径 text_duration=0，
                    // 不含冻结正文时长）。
                    bubble.recompute_hash();
                    TuiRenderUnit::TuiAssistantBubble(bubble)
                } else {
                    let (reasoning, content_hash) = Self::build_bubble_parts(
                        reasoning_slice,
                        self.open_text_hash,
                        self.open_reasoning_hash,
                        // 文本到达后推理已结束：推理块 Completed、正文继续 Running
                        self.trailing_reasoning_frozen_ms.is_none(),
                        self.reasoning_started_at,
                        self.trailing_reasoning_frozen_ms,
                        self.text_started_at,
                    );
                    TuiRenderUnit::TuiAssistantBubble(TuiAssistantBubble {
                        text: text_slice.to_string(),
                        reasoning,
                        message_id: self.last_message_id.clone(),
                        started_at: self.text_started_at,
                        duration_ms: None,
                        content_hash,
                    })
                };
                if self.cached_view_models.len() <= trailing_idx {
                    self.cached_view_models.push_back(trailing);
                } else {
                    self.cached_view_models.set(trailing_idx, trailing);
                }
            }
        }
    }
}
