//! 幂等聚合器（§6.3）。
//!
//! **纯函数**：无 I/O、无日志副作用（脱敏日志由调用方在返回后统一打，§9.3/
//! §12 测试前提）；可重入——同一事件流应用两次视图等价。
//!
//! 判定顺序（§9.2）：epoch 校验 → seq 水位/gap → uncalibratable → chat
//! 终态 → 幂等键 → 终态守卫（interrupted 校准例外，§9.3）→ 关联检查 → 应用
//! （chat 写入 → control 写入，事务顺序固定 chat → control，§6.4/§7.4）。

use yrs::{Array, Map, ReadTxn, Transact, WriteTxn};

use acp_hub_proto::schema::{
    ActiveTurnProjection, EntryKind, EntryRole, EntryStatus, PermissionOptions, PermissionStatus, PublicError,
    ChatStatus, ToolCallProjection, ToolCallStatus, TurnStatus,
};

use crate::state::chat_writer::{self, ContentKind};
use crate::state::doc_pair::{DocPair, StreamState};
use crate::state::factory::ROOT;
use crate::state::normalized::{EventBody, NormalizedEvent};
use crate::state::permission::{self, CasOutcome};
use crate::state::session_list;
use crate::state::view_store::TransactionCtx;

/// 幂等聚合器（§6.3）。无跨调用状态：全部状态在 DocPair 内（§4）。
#[derive(Debug, Default, Clone, Copy)]
pub struct Aggregator;

/// 工具结果截断阈值（§9.5【决策】默认 4KB，对齐 §14 开放问题 2 方向）。
pub const TOOL_RESULT_MAX_BYTES: usize = 4096;

/// 应用结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyResult {
    /// 是否投影成功（false 时为拒绝/幂等跳过）。
    pub applied: bool,
    /// 拒绝原因（applied=false 时；幂等跳过/守卫拒绝）。
    pub reason: Option<ApplyReason>,
}

impl ApplyResult {
    fn applied() -> Self {
        ApplyResult {
            applied: true,
            reason: None,
        }
    }

    fn rejected(reason: ApplyReason) -> Self {
        ApplyResult {
            applied: false,
            reason: Some(reason),
        }
    }
}

/// 拒绝原因（§6.3「拒绝投影并记录脱敏诊断」）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyReason {
    /// 幂等键（turn_id/entry_id/tool_call_id/permission_id）已存在，重放跳过。
    DuplicateIdempotent,
    /// 终态守卫：turn 处于 cancelling/completed/failed/cancelled，晚到增量丢弃
    /// （§6.3）。
    TurnTerminalGuard,
    /// interrupted 状态下：非终态事件丢弃；或终态事件缺重放序依据（§6.3
    /// 例外）。
    InterruptedGuard,
    /// interrupted 校准恰一次：该 turn 已被校准（active_turn 已是实际终态）或
    /// seq 非单调。
    CalibrationDone,
    /// 缺少必要关联信息（关联的 turn/entry/tool_call/permission 未知）。
    UnknownTurn,
    /// tool_call_id 未知。
    UnknownToolCall,
    /// permission_id 未知。
    UnknownPermission,
    /// 防御性：epoch 与当前流不一致（§4.5.1 帧直接丢弃并计数）。
    EpochMismatch,
    /// 防御性：seq 回退（低于 last_seq；补推纪律下不应出现，§8.5）。
    SeqOutOfOrder,
    /// chat 已终态（ended/closed/crashed），拒绝新事件（§8.2）。
    ChatClosed,
    /// 不可校准缺口存在时的补推事件（epoch 变化路径，§8.5）——拒绝除
    /// `session/load` 显式重建（F7 命令路径）外的一切投影。
    UncalibratableGap,
    /// TurnTerminal 携带非终态状态值（§3.2【决策】防御：状态仅限终态四值）。
    InvalidTerminalStatus,
}

/// control doc 只读快照（批次内稳定：delta 不写 control）。
#[derive(Debug, Clone, Default)]
struct ControlSnapshot {
    /// chat.status（ended/closed/crashed → 拒绝新事件，§8.2）。
    chat_closed: bool,
    /// active_turn 投影（终态守卫依据，§6.3）。
    active_turn: Option<ActiveTurnProjection>,
}

impl Aggregator {
    /// 应用单个事件（纯函数）。自管理事务：chat 一次、control 一次，顺序固定
    /// chat → control（§6.4/§7.4）；不涉及某 doc 时跳过该事务。
    pub fn apply(&mut self, pair: &mut DocPair, ev: &NormalizedEvent) -> ApplyResult {
        // 判定（只读，含 stream 状态推进）。
        if let Err(reason) = self.judge(pair, ev) {
            return ApplyResult::rejected(reason);
        }
        // 应用：chat → control。
        self.write(pair, ev);
        ApplyResult::applied()
    }

    /// 微批次入口（§6.4/§8.3）：delta 类事件共享一次 chat 事务；非 delta
    /// 事件（防御）回落单事件路径。返回逐事件结果。
    pub fn apply_batch(&mut self, pair: &mut DocPair, evs: &[NormalizedEvent]) -> Vec<ApplyResult> {
        let mut results = Vec::with_capacity(evs.len());
        let mut i = 0;
        while i < evs.len() {
            let is_delta = matches!(
                evs[i].body,
                EventBody::MessageDelta { .. } | EventBody::ReasoningDelta { .. }
            );
            if !is_delta {
                // 防御：非 delta 事件不进批次（控制类应先 flush）；独立事务。
                results.push(self.apply(pair, &evs[i]));
                i += 1;
                continue;
            }
            // 连续 delta 段：共享一次 chat 事务（§6.4/§8.3 微批次合并）。
            let mut end = i;
            while end < evs.len()
                && matches!(
                    evs[end].body,
                    EventBody::MessageDelta { .. } | EventBody::ReasoningDelta { .. }
                )
            {
                end += 1;
            }
            // 字段级拆分借用：chat 写事务与 control 只读快照并存（不同 doc，
            // 无并发事务冲突，§7.4）。
            let chat = &mut pair.chat;
            let control = &pair.session;
            let mut txn = chat.transact_mut();
            let snapshot = self.read_control_snapshot(control);
            let mut applied_in_segment = false;
            for ev in &evs[i..end] {
                let r = self.apply_delta_in_txn(&mut pair.stream, &mut txn, ev, &snapshot);
                if r.applied {
                    applied_in_segment = true;
                }
                results.push(r);
            }
            // txn drop = 段批次单事务提交。
            drop(txn);
            // §7.2 状态推进：内容增量到达 → accepting → running（参考实现：
            // 首条内容增量即 turn 开始运行）。段内任一增量应用成功即推进；
            // 回放模式跳过（回放 turn 由 EndLoadReplay 终态化）。共享一次
            // session 事务。
            if applied_in_segment && !pair.stream.replay_active {
                let mut txn = pair.session_txn();
                let root = txn.get_or_insert_map(ROOT);
                if chat_writer::set_active_turn_status_if(&mut txn, &root, "accepting", "running") {
                    chat_writer::bump_projection_version(&mut txn, &root);
                }
            }
            i = end;
        }
        results
    }

    fn read_control_snapshot(&self, session: &yrs::Doc) -> ControlSnapshot {
        let txn = session.transact();
        let mut snap = ControlSnapshot::default();
        let Some(root) = chat_writer::root_map_read(&txn) else {
            return snap;
        };
        // session map：status（chat 终态）与 active turn 内嵌字段。
        let sm = root
            .get(&txn, "session")
            .and_then(|v| v.cast::<yrs::MapRef>().ok());
        // status。
        snap.chat_closed = sm
            .as_ref()
            .and_then(|m| m.get(&txn, "status"))
            .and_then(|s| s.cast::<String>().ok())
            .map(|s| {
                chat_status_from_str(&s)
                    .map(|st| {
                        matches!(
                            st,
                            ChatStatus::Ended | ChatStatus::Closed | ChatStatus::Crashed
                        )
                    })
                    .unwrap_or(false)
            })
            .unwrap_or(false);
        // active_turn（session map 内嵌：active_turn_id/status/updated_at）。
        snap.active_turn = sm.as_ref().and_then(|m| {
            let str_or = |k: &str| -> Option<String> {
                m.get(&txn, k).and_then(|v| v.cast::<String>().ok())
            };
            Some(ActiveTurnProjection {
                turn_id: str_or("active_turn_id")?,
                turn_status: str_or("active_turn_status")
                    .as_deref()
                    .map(turn_status_from_str)
                    .unwrap_or(TurnStatus::Accepting),
                updated_at: str_or("active_turn_updated_at").unwrap_or_default(),
            })
        });
        snap
    }

    /// delta 事件在共享 chat 事务内判定 + 写入（批次路径）。
    fn apply_delta_in_txn(
        &self,
        stream: &mut StreamState,
        txn: &mut TransactionCtx<'_>,
        ev: &NormalizedEvent,
        snapshot: &ControlSnapshot,
    ) -> ApplyResult {
        // 判定：epoch/seq/gap（步骤 1/2/3）。
        if let Err(reason) = self.judge_stream(stream, ev) {
            return ApplyResult::rejected(reason);
        }
        // 判定：chat 终态（步骤 4）。
        if snapshot.chat_closed {
            return ApplyResult::rejected(ApplyReason::ChatClosed);
        }
        // 判定：终态守卫（步骤 5）+ 关联检查（步骤 6，delta 自动建 entry）。
        // 回放模式（§8.5）无宿主驱动 active_turn——守卫跳过（归位由
        // resolve 按回放 turn 处理）。
        if !stream.replay_active {
            if let Err(reason) = self.judge_turn_guard(snapshot.active_turn.as_ref(), &ev.body) {
                return ApplyResult::rejected(reason);
            }
        }
        // 写入（chat doc，共享事务内）。
        let root = txn.get_or_insert_map(ROOT);
        // 回放合成（§8.5 REPLAY_NEEDS_TURN）：历史首帧即为 agent 增量且无
        // 活动回放 turn → 先合成空文本 user 占位 turn（参考实现规则；避免
        // 空 id 垃圾条目）。合成后归位到该 turn。
        if stream.replay_active && stream.replay_turn.is_none() {
            let t = format!("load:{}", ev.seq);
            stream.replay_turn = Some(t.clone());
            stream.replay_turns.push(t.clone());
            chat_writer::create_user_entry(txn, &root, &t, &format!("{t}:user"), "", None, &ev.ts);
            chat_writer::bump_projection_version(txn, &root);
        }
        match &ev.body {
            EventBody::MessageDelta {
                turn_id,
                entry_id,
                block_id,
                text,
            } => {
                let (turn_id, entry_id, block_id) =
                    self.resolve_entry_ids_from_snapshot(stream, snapshot, turn_id, entry_id, block_id, "text");
                chat_writer::ensure_entry_with_blocks(
                    txn,
                    &root,
                    &entry_id,
                    EntryKind::Message,
                    EntryRole::Assistant,
                    Some(&turn_id),
                    "",
                );
                chat_writer::append_text_delta(txn, &root, &entry_id, &block_id, text, ContentKind::Text);
                chat_writer::bump_projection_version(txn, &root);
            }
            EventBody::ReasoningDelta {
                turn_id,
                entry_id,
                block_id,
                text,
                visibility,
            } => {
                let (turn_id, entry_id, block_id) = self.resolve_entry_ids_from_snapshot(
                    stream,
                    snapshot,
                    turn_id,
                    entry_id,
                    block_id,
                    "reasoning",
                );
                chat_writer::ensure_entry_with_blocks(
                    txn,
                    &root,
                    &entry_id,
                    EntryKind::Message,
                    EntryRole::Assistant,
                    Some(&turn_id),
                    "",
                );
                chat_writer::append_text_delta(
                    txn,
                    &root,
                    &entry_id,
                    &block_id,
                    text,
                    ContentKind::Reasoning,
                );
                chat_writer::set_reasoning_visibility(txn, &root, &block_id, *visibility);
                chat_writer::bump_projection_version(txn, &root);
            }
            _ => unreachable!("batch 只含 delta（防御已过滤）"),
        }
        ApplyResult::applied()
    }

    /// 批次快照路径的增量归位（同 [`Self::resolve_entry_ids`]，从批次快照的
    /// active_turn 读取，避免重读 control doc）。回放模式（§8.5）优先归位
    /// 到回放 turn。
    fn resolve_entry_ids_from_snapshot(
        &self,
        stream: &StreamState,
        snapshot: &ControlSnapshot,
        turn_id: &str,
        entry_id: &str,
        block_id: &str,
        block_kind: &str,
    ) -> (String, String, String) {
        if !turn_id.is_empty() {
            return (
                turn_id.to_string(),
                entry_id.to_string(),
                block_id.to_string(),
            );
        }
        if let Some(rt) = stream.replay_turn.as_ref() {
            return (
                rt.clone(),
                format!("{rt}:assistant"),
                block_kind.to_string(),
            );
        }
        match snapshot.active_turn.as_ref() {
            Some(a) => (
                a.turn_id.clone(),
                format!("{}:assistant", a.turn_id),
                block_kind.to_string(),
            ),
            None => (String::new(), String::new(), String::new()),
        }
    }

    /// 判定（§9.2 顺序）。只读 doc + 推进 pair.stream；返回拒绝原因。
    fn judge(&self, pair: &mut DocPair, ev: &NormalizedEvent) -> Result<(), ApplyReason> {
        // 1/2/3. epoch/seq/gap/uncalibratable。
        self.judge_stream(&mut pair.stream, ev)?;
        // 4. chat 终态（§8.2）：读 session doc session map status。
        {
            let txn = pair.session.transact();
            if let Some(root) = chat_writer::root_map_read(&txn) {
                let status = root
                    .get(&txn, "session")
                    .and_then(|v| v.cast::<yrs::MapRef>().ok())
                    .and_then(|m| m.get(&txn, "status"))
                    .and_then(|s| s.cast::<String>().ok());
                if let Some(s) = status {
                    if let Some(st) = chat_status_from_str(&s) {
                        if matches!(st, ChatStatus::Ended | ChatStatus::Closed | ChatStatus::Crashed) {
                            return Err(ApplyReason::ChatClosed);
                        }
                    }
                }
            }
        }
        // 5. 终态守卫（§6.3/§7.2/§9.3）：读 control doc active_turn。
        let active = self.read_active_turn(pair);
        // 6. 幂等键（§6.3）与关联检查（按事件体分派）。
        self.judge_body(pair, ev, active.as_ref())?;
        Ok(())
    }

    /// 判定步骤 1/2/3：epoch 校验、seq 水位 + gap、uncalibratable（§9.2）。
    fn judge_stream(&self, stream: &mut StreamState, ev: &NormalizedEvent) -> Result<(), ApplyReason> {
        // 1. epoch 校验（§4.5.1 防御；正常路径 hello 已对账）。
        if ev.epoch != stream.epoch {
            if ev.epoch > stream.epoch {
                // 新 chat 首事件（instance 新开 chat epoch=1，§4.5.1）
                // 为**基线采纳**：流尚无任何事件（last_seq=0 且无既有 gap），
                // 不存在缓冲丢失可能，采纳 epoch 后正常应用本帧，不置不可
                // 校准缺口（hello 对账对新建 chat 的落点；修复
                // relay_event_handler_test 已知缺口注释记录的「epoch=1 首
                // 事件触发 uncalibratable 缺口」）。
                let fresh_baseline =
                    stream.last_seq == 0 && stream.gap_count == 0 && !stream.gap_dirty;
                stream.epoch = ev.epoch;
                if !fresh_baseline {
                    // 既有流上的合法新纪元（daemon 重启/进程重建，§8.5）：
                    // 置不可校准缺口并拒绝本帧（补推契约失效）。
                    stream.uncalibratable = true;
                    stream.gap_dirty = true;
                    return Err(ApplyReason::EpochMismatch);
                }
            } else {
                return Err(ApplyReason::EpochMismatch);
            }
        }
        // 2. seq 水位 + gap 计算（§8.5/§9.4）。
        if ev.seq <= stream.last_seq {
            return Err(ApplyReason::SeqOutOfOrder);
        }
        let expected = stream.last_seq + 1;
        if ev.seq > expected {
            stream.gap_count += ev.seq - expected;
            stream.gap_dirty = true;
        } else if stream.gap_count > 0 {
            // 追平：无缺口且此前有 gap → 清零（§9.4）。
            stream.gap_count = 0;
            stream.gap_dirty = true;
        }
        stream.last_seq = ev.seq;
        // 3. 不可校准缺口：拒绝一切投影（重建经 F7 命令路径）。
        if stream.uncalibratable {
            return Err(ApplyReason::UncalibratableGap);
        }
        Ok(())
    }

    fn read_active_turn(&self, pair: &DocPair) -> Option<ActiveTurnProjection> {
        let txn = pair.session.transact();
        let root = chat_writer::root_map_read(&txn)?;
        let sm = root.get(&txn, "session")?.cast::<yrs::MapRef>().ok()?;
        let str_or = |k: &str| -> Option<String> {
            sm.get(&txn, k).and_then(|v| v.cast::<String>().ok())
        };
        Some(ActiveTurnProjection {
            turn_id: str_or("active_turn_id")?,
            turn_status: str_or("active_turn_status")
                .as_deref()
                .map(turn_status_from_str)
                .unwrap_or(TurnStatus::Accepting),
            updated_at: str_or("active_turn_updated_at").unwrap_or_default(),
        })
    }

    /// 增量帧 id 归位（§7.2 宿主驱动 turn 模型）：帧携带 turn_id（acp-hub
    /// 私有帧/test-child）原样使用；帧无 id（真实 peri agent_message_chunk /
    /// agent_thought_chunk，无 turnId/entryId/blockId）按 active_turn 归位：
    /// entry_id = `{active_turn}:assistant`（与 TurnTerminal 派生一致），
    /// block_id = 内容块种类（text/reasoning，entry 内单块）。
    fn resolve_entry_ids(
        &self,
        pair: &DocPair,
        turn_id: &str,
        entry_id: &str,
        block_id: &str,
        block_kind: &str,
    ) -> (String, String, String) {
        if !turn_id.is_empty() {
            return (
                turn_id.to_string(),
                entry_id.to_string(),
                block_id.to_string(),
            );
        }
        // 回放模式（§8.5）：优先归位到回放 turn。
        if let Some(rt) = pair.stream.replay_turn.as_ref() {
            return (
                rt.clone(),
                format!("{rt}:assistant"),
                block_kind.to_string(),
            );
        }
        let active = self.read_active_turn(pair);
        match active {
            Some(a) => (
                a.turn_id.clone(),
                format!("{}:assistant", a.turn_id),
                block_kind.to_string(),
            ),
            None => (String::new(), String::new(), String::new()),
        }
    }

    /// 幂等键 + 关联检查 + 终态守卫（步骤 5/6 合并：守卫依赖 active_turn）。
    fn judge_body(
        &self,
        pair: &mut DocPair,
        ev: &NormalizedEvent,
        active: Option<&ActiveTurnProjection>,
    ) -> Result<(), ApplyReason> {
        match &ev.body {
            EventBody::MessageDelta { entry_id, .. }
            | EventBody::ReasoningDelta { entry_id, .. } => {
                if !pair.stream.replay_active {
                    self.judge_turn_guard(active, &ev.body)?;
                }
                // 关联：entry 未知自动建（§7 注释），不拒绝。
                let _ = entry_id;
                Ok(())
            }
            EventBody::UserMessage {
                turn_id,
                entry_id,
                ..
            } => {
                // `session/load` 回放（§8.5）：历史 user 消息同样无 turn_id，
                // 但回放是**显式重建**——聚合器按回放序生成 turn 归位
                // （`load:{seq}`，seq 水位单调 → 天然幂等，见 write 分支），
                // 不拒绝。
                if pair.stream.replay_active {
                    return Ok(());
                }
                // ACP 回显无 turn_id（真实 peri 不回声 user_message_chunk；
                // 防御：空 id 不创建 entry）——用户消息由服务端单写注入
                // （§6.5 RegisterUserEntry，携带 server 生成 turn_id）。
                if turn_id.is_empty() || entry_id.is_empty() {
                    return Err(ApplyReason::UnknownTurn);
                }
                // 幂等：同 turn_id/entry_id 的 user entry 已存在 → 跳过（§6.5）。
                let txn = pair.chat.transact();
                if chat_writer::entry_exists(&txn, entry_id)
                    || self.user_entry_for_turn_exists(&txn, turn_id)
                {
                    return Err(ApplyReason::DuplicateIdempotent);
                }
                Ok(())
            }
            EventBody::ToolCallStarted { tool_call_id, .. } => {
                // 幂等：tool_call_id 已存在 → 跳过。
                let txn = pair.chat.transact();
                if chat_writer::tool_call_exists(&txn, tool_call_id) {
                    return Err(ApplyReason::DuplicateIdempotent);
                }
                if !pair.stream.replay_active {
                    self.judge_turn_guard(active, &ev.body)?;
                }
                Ok(())
            }
            EventBody::ToolCallUpdated { tool_call_id, .. } => {
                if !pair.stream.replay_active {
                    self.judge_turn_guard(active, &ev.body)?;
                }
                let txn = pair.chat.transact();
                if !chat_writer::tool_call_exists(&txn, tool_call_id) {
                    return Err(ApplyReason::UnknownToolCall);
                }
                Ok(())
            }
            EventBody::ToolCallCompleted { tool_call_id, .. } => {
                if !pair.stream.replay_active {
                    self.judge_turn_guard(active, &ev.body)?;
                }
                let txn = pair.chat.transact();
                if !chat_writer::tool_call_exists(&txn, tool_call_id) {
                    return Err(ApplyReason::UnknownToolCall);
                }
                Ok(())
            }
            EventBody::PermissionRequested {
                permission_id,
                ..
            } => {
                // 幂等：permission_id 已存在 → 跳过。
                let txn = pair.session.transact();
                if self.permission_exists(&txn, permission_id) {
                    return Err(ApplyReason::DuplicateIdempotent);
                }
                self.judge_turn_guard(active, &ev.body)
            }
            EventBody::PermissionResolved { permission_id, .. } => {
                let txn = pair.session.transact();
                if !self.permission_exists(&txn, permission_id) {
                    return Err(ApplyReason::UnknownPermission);
                }
                Ok(())
            }
            EventBody::PermissionExpired { permission_id } => {
                let txn = pair.session.transact();
                if !self.permission_exists(&txn, permission_id) {
                    return Err(ApplyReason::UnknownPermission);
                }
                Ok(())
            }
            EventBody::AgentStatus { .. }
            | EventBody::Capabilities { .. }
            | EventBody::SessionInfo { .. }
            | EventBody::SessionListResponse { .. } => Ok(()),
            EventBody::TurnTerminal {
                turn_id,
                status,
                ..
            } => {
                // 防御：仅终态四值（§3.2【决策】）。
                if !matches!(
                    status,
                    TurnStatus::Completed
                        | TurnStatus::Failed
                        | TurnStatus::Cancelled
                        | TurnStatus::Interrupted
                ) {
                    return Err(ApplyReason::InvalidTerminalStatus);
                }
                // 终态守卫：active_turn 决定应用/拒绝。
                match active {
                    None => Err(ApplyReason::UnknownTurn),
                    Some(a) if a.turn_id != *turn_id => Err(ApplyReason::TurnTerminalGuard),
                    Some(a) => match a.turn_status {
                        // 不可逆终态：终态事件二次到达 → 校准完成（§7.2）。
                        TurnStatus::Completed
                        | TurnStatus::Failed
                        | TurnStatus::Cancelled => Err(ApplyReason::CalibrationDone),
                        // cancelling：终态事件应用（状态机迁移，§7.2）；非终态
                        // 事件由 judge_turn_guard 拒绝。
                        TurnStatus::Cancelling => Ok(()),
                        // interrupted：校准例外（§6.3/§9.3 双条件：状态位 +
                        // 重放序单调；单调已由步骤 2 保证）。
                        TurnStatus::Interrupted => Ok(()),
                        _ => Ok(()),
                    },
                }
            }
        }
    }

    /// 终态守卫（§6.3）：active_turn 非活动（cancelling/不可逆终态/interrupted）
    /// 时拒绝带 turn_id 的非终态事件；interrupted 时拒绝一切非终态事件。
    /// 供批次快照路径（不重读 control doc）。
    fn judge_turn_guard(
        &self,
        active: Option<&ActiveTurnProjection>,
        body: &EventBody,
    ) -> Result<(), ApplyReason> {
        let turn_id = match body {
            EventBody::MessageDelta { turn_id, .. }
            | EventBody::ReasoningDelta { turn_id, .. }
            | EventBody::ToolCallStarted { turn_id, .. }
            | EventBody::ToolCallUpdated { turn_id, .. }
            | EventBody::ToolCallCompleted { turn_id, .. }
            | EventBody::PermissionRequested { turn_id, .. } => Some(turn_id.as_str()),
            _ => None,
        };
        match (active, turn_id) {
            // 事件不带 turn（AgentStatus/Capabilities/SessionInfo/...）：无守卫。
            (_, None) => Ok(()),
            // 帧无 turn_id（真实 peri 增量，§7.2 宿主驱动 turn 模型；照抄
            // @fenix/chat-channel canWriteToTurn）：按 active_turn 归位校验——
            // 无活动 turn → 未知 turn；活动 turn 终态/cancelling → 拒绝。
            (None, Some("")) => Err(ApplyReason::UnknownTurn),
            (Some(a), Some("")) => match a.turn_status {
                TurnStatus::Cancelling
                | TurnStatus::Completed
                | TurnStatus::Failed
                | TurnStatus::Cancelled => Err(ApplyReason::TurnTerminalGuard),
                TurnStatus::Interrupted => Err(ApplyReason::InterruptedGuard),
                _ => Ok(()),
            },
            // 带 turn 但无 active_turn：turn 未知（§9.2 步骤 6）。
            (None, Some(_)) => Err(ApplyReason::UnknownTurn),
            (Some(a), Some(tid)) if a.turn_id != tid => Err(ApplyReason::TurnTerminalGuard),
            (Some(a), Some(_)) => match a.turn_status {
                TurnStatus::Cancelling
                | TurnStatus::Completed
                | TurnStatus::Failed
                | TurnStatus::Cancelled => Err(ApplyReason::TurnTerminalGuard),
                TurnStatus::Interrupted => Err(ApplyReason::InterruptedGuard),
                _ => Ok(()),
            },
        }
    }

    fn user_entry_for_turn_exists<T: ReadTxn>(&self, txn: &T, turn_id: &str) -> bool {
        chat_writer::root_map_read(txn)
            .and_then(|root| root.get(txn, "entries"))
            .and_then(|v| v.cast::<yrs::MapRef>().ok())
            .map(|entries| {
                entries.iter(txn).any(|(_, v)| {
                    v.cast::<yrs::MapRef>().ok().map(|m| {
                        m.get(txn, "role")
                            .and_then(|r| r.cast::<String>().ok())
                            .as_deref()
                            == Some("user")
                            && m.get(txn, "turn_id")
                                .and_then(|t| t.cast::<String>().ok())
                                .as_deref()
                                == Some(turn_id)
                    }) == Some(true)
                })
            })
            .unwrap_or(false)
    }

    fn permission_exists<T: ReadTxn>(&self, txn: &T, permission_id: &str) -> bool {
        chat_writer::root_map_read(txn)
            .and_then(|root| root.get(txn, "pending_permissions"))
            .and_then(|v| v.cast::<yrs::MapRef>().ok())
            .map(|perms| perms.get(txn, permission_id).is_some())
            .unwrap_or(false)
    }

    /// 应用（写入）：chat → control 顺序（§6.4）。
    ///
    /// 事务纪律：禁止跨 await 持有（§7.4）；同一 doc 的并发事务会 panic，
    /// 故预读（upsert 保留字段）在开写事务前完成，CAS 类写入自开事务。
    fn write(&self, pair: &mut DocPair, ev: &NormalizedEvent) {
        // 预读：chat 侧 upsert 需要保留的现有投影（开写事务前完成）。
        let pre_read = match &ev.body {
            EventBody::ToolCallUpdated { tool_call_id, .. }
            | EventBody::ToolCallCompleted { tool_call_id, .. } => {
                let txn = pair.chat.transact();
                chat_writer::tool_call_projection(&txn, tool_call_id)
            }
            _ => None,
        };
        // 预读：回放合成（§8.5 REPLAY_NEEDS_TURN）——历史首帧即为 agent
        // 增量（无 user 消息先行）时无 turn 可归位，先分配回放归位 turn
        // （`load:{seq}`，seq 水位单调天然幂等）；user 占位 entry 在 chat
        // 事务内创建（参考实现：合成一条空文本 user_message 回放 turn，
        // 避免空 id 垃圾条目）。
        let replay_active = pair.stream.replay_active;
        let replay_synth = if replay_active
            && pair.stream.replay_turn.is_none()
            && matches!(
                ev.body,
                EventBody::MessageDelta { .. } | EventBody::ReasoningDelta { .. }
            ) {
            let t = format!("load:{}", ev.seq);
            pair.stream.replay_turn = Some(t.clone());
            pair.stream.replay_turns.push(t.clone());
            Some((t.clone(), format!("{t}:user")))
        } else {
            None
        };
        // 预读：帧无 id（真实 peri 增量）按 active_turn 归位（§7.2；读
        // control doc 须在 chat 写事务之前，§7.4 借位纪律）。
        let resolved = match &ev.body {
            EventBody::MessageDelta {
                turn_id,
                entry_id,
                block_id,
                ..
            } => Some(self.resolve_entry_ids(pair, turn_id, entry_id, block_id, "text")),
            EventBody::ReasoningDelta {
                turn_id,
                entry_id,
                block_id,
                ..
            } => Some(self.resolve_entry_ids(pair, turn_id, entry_id, block_id, "reasoning")),
            _ => None,
        };
        // 预读：回放模式（§8.5）历史 user 消息的归位 turn（chat 事务前
        // 计算——事务借用与 stream 可变借用互斥，§7.4）。
        let replay = if pair.stream.replay_active {
            match &ev.body {
                EventBody::UserMessage { .. } => {
                    let t = format!("load:{}", ev.seq);
                    pair.stream.replay_turn = Some(t.clone());
                    pair.stream.replay_turns.push(t.clone());
                    Some((t.clone(), format!("{t}:user")))
                }
                _ => None,
            }
        } else {
            None
        };
        // chat 侧写入（一次事务）。
        {
            let mut txn = pair.chat_txn();
            let root = txn.get_or_insert_map(ROOT);
            // 回放合成占位 user entry（预读区已分配 turn，§8.5）。
            if let Some((t, e)) = &replay_synth {
                chat_writer::create_user_entry(&mut txn, &root, t, e, "", None, &ev.ts);
                chat_writer::bump_projection_version(&mut txn, &root);
            }
            match &ev.body {
                EventBody::MessageDelta { text, .. } => {
                    // 帧无 id（真实 peri 增量）：按 active_turn 归位（§7.2；
                    // chat-channel ASSISTANT_ENTRY 派生规则）。
                    let (turn_id, entry_id, block_id) = resolved.clone().unwrap();
                    chat_writer::ensure_entry_with_blocks(
                        &mut txn,
                        &root,
                        &entry_id,
                        EntryKind::Message,
                        EntryRole::Assistant,
                        Some(&turn_id),
                        "",
                    );
                    chat_writer::append_text_delta(
                        &mut txn,
                        &root,
                        &entry_id,
                        &block_id,
                        text,
                        ContentKind::Text,
                    );
                    chat_writer::bump_projection_version(&mut txn, &root);
                }
                EventBody::ReasoningDelta {
                    text, visibility, ..
                } => {
                    let (turn_id, entry_id, block_id) = resolved.clone().unwrap();
                    chat_writer::ensure_entry_with_blocks(
                        &mut txn,
                        &root,
                        &entry_id,
                        EntryKind::Message,
                        EntryRole::Assistant,
                        Some(&turn_id),
                        "",
                    );
                    chat_writer::append_text_delta(
                        &mut txn,
                        &root,
                        &entry_id,
                        &block_id,
                        text,
                        ContentKind::Reasoning,
                    );
                    chat_writer::set_reasoning_visibility(
                        &mut txn,
                        &root,
                        &block_id,
                        *visibility,
                    );
                    chat_writer::bump_projection_version(&mut txn, &root);
                }
                EventBody::UserMessage {
                    turn_id,
                    entry_id,
                    text,
                    author_user_id,
                    created_at,
                } => {
                    // 回放模式（§8.5）：历史 user 消息无 turn_id——按回放序
                    // 生成归位 turn（`load:{seq}`；seq 水位单调，天然幂等），
                    // 后续 agent chunk 归位到该 turn。turn 在预读区计算。
                    let (replay_turn, replay_entry) = match &replay {
                        Some((t, e)) => (t.clone(), e.clone()),
                        None => (turn_id.clone(), entry_id.clone()),
                    };
                    chat_writer::create_user_entry(
                        &mut txn,
                        &root,
                        &replay_turn,
                        &replay_entry,
                        text,
                        author_user_id.as_deref(),
                        created_at,
                    );
                    chat_writer::bump_projection_version(&mut txn, &root);
                }
                EventBody::ToolCallStarted {
                    turn_id,
                    tool_call_id,
                    name,
                    arguments,
                    ..
                } => {
                    let tc = ToolCallProjection {
                        tool_call_id: tool_call_id.clone(),
                        turn_id: turn_id.clone(),
                        name: name.clone(),
                        status: ToolCallStatus::Pending,
                        arguments: arguments.clone(),
                        result: None,
                        public_error: None,
                        permission_id: None,
                    };
                    chat_writer::upsert_tool_call(&mut txn, &root, &tc);
                    chat_writer::bump_projection_version(&mut txn, &root);
                }
                EventBody::ToolCallUpdated {
                    arguments,
                    ..
                } => {
                    // 现有投影上 arguments 全量覆盖（M1，§3.2）。
                    let mut tc = pre_read.clone().unwrap_or_else(default_tool_call);
                    tc.arguments = arguments.clone();
                    chat_writer::upsert_tool_call(&mut txn, &root, &tc);
                    chat_writer::bump_projection_version(&mut txn, &root);
                }
                EventBody::ToolCallCompleted {
                    result,
                    public_error,
                    ..
                } => {
                    let mut tc = pre_read.clone().unwrap_or_else(default_tool_call);
                    tc.status = if public_error.is_some() {
                        ToolCallStatus::Error
                    } else {
                        ToolCallStatus::Completed
                    };
                    // 超大 result 截断（§9.5：只做大小预算，不写超限内容）。
                    tc.result = result
                        .clone()
                        .filter(|v| {
                            serde_json::to_vec(v)
                                .map(|b| b.len() <= TOOL_RESULT_MAX_BYTES)
                                .unwrap_or(false)
                        });
                    tc.public_error = public_error.clone();
                    chat_writer::upsert_tool_call(&mut txn, &root, &tc);
                    chat_writer::bump_projection_version(&mut txn, &root);
                }
                EventBody::TurnTerminal {
                    turn_id,
                    status,
                    completed_at,
                    public_error,
                } => {
                    // Chat 侧：assistant entry 终态迁移（§7.2）。
                    let entry_id = format!("{turn_id}:assistant");
                    let entry_status = match status {
                        TurnStatus::Completed => EntryStatus::Completed,
                        TurnStatus::Failed => EntryStatus::Error,
                        TurnStatus::Cancelled | TurnStatus::Interrupted => EntryStatus::Cancelled,
                        _ => unreachable!("judge 已拒绝非终态"),
                    };
                    chat_writer::migrate_entry_terminal(
                        &mut txn,
                        &root,
                        &entry_id,
                        entry_status,
                        completed_at,
                        public_error.as_ref(),
                    );
                    chat_writer::bump_projection_version(&mut txn, &root);
                }
                // 不涉 chat doc 的事件：无 chat 写入。
                EventBody::PermissionRequested { .. }
                | EventBody::PermissionResolved { .. }
                | EventBody::PermissionExpired { .. }
                | EventBody::AgentStatus { .. }
                | EventBody::Capabilities { .. }
                | EventBody::SessionInfo { .. }
                | EventBody::SessionListResponse { .. } => {}
            }
        }
        // control 侧写入：CAS 类自开事务（permission 原语内部管理），其余在
        // 一次 control 事务内；须在 chat 事务 drop 后（§6.4 固定顺序）。
        match &ev.body {
            EventBody::PermissionResolved {
                permission_id,
                decision,
            } => {
                // CAS：pending → resolved 原子一次（§7.4 规则 4）；Migrated 时
                // 由原语写入 decision 与状态，聚合器补 bump。
                if permission::resolve(pair, permission_id, *decision) == CasOutcome::Migrated {
                    // §7.2 状态推进：决议后无其他 pending → awaitingPermission
                    // → running（参考实现 resolve(allow) 语义）。计数在写事务
                    // 前完成（yrs 同 doc 事务互斥）。
                    let no_pending_left = permission::pending_count(pair) == 0;
                    let mut txn = pair.session_txn();
                    let root = txn.get_or_insert_map(ROOT);
                    if no_pending_left {
                        chat_writer::set_active_turn_status_if(&mut txn, &root, "awaitingPermission", "running");
                    }
                    chat_writer::bump_projection_version(&mut txn, &root);
                }
            }
            EventBody::PermissionExpired { permission_id } => {
                if permission::expire(pair, permission_id) == CasOutcome::Migrated {
                    // §7.2 状态推进：无其他 pending → awaitingPermission →
                    // cancelled（参考实现 expire 语义：未决议权限过期即 turn
                    // 取消）。
                    let no_pending_left = permission::pending_count(pair) == 0;
                    let mut txn = pair.session_txn();
                    let root = txn.get_or_insert_map(ROOT);
                    if no_pending_left {
                        // 未决议权限全部过期 → 该 turn 取消（参考实现 expire
                        // 语义）；仅当状态仍为 awaitingPermission 时推进。
                        chat_writer::set_active_turn_status_if(&mut txn, &root, "awaitingPermission", "cancelled");
                    }
                    chat_writer::bump_projection_version(&mut txn, &root);
                }
            }
            EventBody::SessionListResponse { entries } => {
                // 预读（写事务前完成，避免并发事务 panic，§7.4）。
                let current = {
                    let rt = pair.session.transact();
                    match chat_writer::root_map_read(&rt) {
                        Some(rr) => session_list::read_current(&rt, &rr),
                        None => std::collections::HashMap::new(),
                    }
                };
                let d = session_list::diff(&current, entries);
                if !d.upsert.is_empty() || !d.remove.is_empty() {
                    let mut txn = pair.session_txn();
                    let root = txn.get_or_insert_map(ROOT);
                    session_list::apply_diff(&mut txn, &root, &d);
                    chat_writer::bump_projection_version(&mut txn, &root);
                }
            }
            _ => {
                let mut txn = pair.session_txn();
                let root = txn.get_or_insert_map(ROOT);
                match &ev.body {
                    EventBody::SessionListResponse { .. } => {
                        unreachable!("SessionListResponse 已在外层分支处理")
                    }
                    EventBody::UserMessage {
                        turn_id,
                        created_at,
                        ..
                    } => {
                        // active_turn 注册（§7.2：turn 从 accepting 开始）。
                        let active = ActiveTurnProjection {
                            turn_id: turn_id.clone(),
                            turn_status: TurnStatus::Accepting,
                            updated_at: created_at.clone(),
                        };
                        chat_writer::set_active_turn(&mut txn, &root, Some(&active));
                        chat_writer::bump_projection_version(&mut txn, &root);
                    }
                    EventBody::PermissionRequested {
                        permission_id,
                        turn_id,
                        tool_call_id,
                        title,
                        description,
                        options,
                        expires_at,
                    } => {
                        write_permission_request(
                            &mut txn,
                            &root,
                            permission_id,
                            turn_id,
                            tool_call_id.as_deref(),
                            title,
                            description.as_deref(),
                            options,
                            expires_at,
                        );
                        // §7.2 状态推进：权限请求发出 → 宿主等待决议
                        // （accepting/running → awaitingPermission；参考状态机
                        // accepting → running ⇄ awaitingPermission）。仅当请求
                        // 关联当前 active turn 时推进（防御：无关 turn 的请求
                        // 不改状态）。
                        let active_tid = root
                            .get(&txn, "session")
                            .and_then(|v| v.cast::<yrs::MapRef>().ok())
                            .and_then(|m| m.get(&txn, "active_turn_id"))
                            .and_then(|t| t.cast::<String>().ok());
                        if active_tid.as_deref() == Some(turn_id.as_str())
                            && !chat_writer::set_active_turn_status_if(
                                &mut txn,
                                &root,
                                "accepting",
                                "awaitingPermission",
                            )
                            && !chat_writer::set_active_turn_status_if(
                                &mut txn,
                                &root,
                                "running",
                                "awaitingPermission",
                            )
                        {
                            // 既非 accepting 也非 running：无状态推进（防御）。
                        }
                        chat_writer::bump_projection_version(&mut txn, &root);
                    }
                    EventBody::AgentStatus {
                        status,
                        public_error,
                    } => {
                        write_agent_status(&mut txn, &root, status, public_error.as_ref());
                        chat_writer::bump_projection_version(&mut txn, &root);
                    }
                    EventBody::Capabilities { capabilities } => {
                        write_capabilities(&mut txn, &root, capabilities);
                        chat_writer::bump_projection_version(&mut txn, &root);
                    }
                    EventBody::SessionInfo {
                        title,
                        status,
                        active_turn_id,
                    } => {
                        write_chat_info(
                            &mut txn,
                            &root,
                            title.as_deref(),
                            *status,
                            active_turn_id.as_deref(),
                        );
                        chat_writer::bump_projection_version(&mut txn, &root);
                    }
                            EventBody::TurnTerminal {
                        turn_id,
                        status,
                        completed_at,
                        ..
                    } => {
                        let active = ActiveTurnProjection {
                            turn_id: turn_id.clone(),
                            turn_status: *status,
                            updated_at: completed_at.clone(),
                        };
                        chat_writer::set_active_turn(&mut txn, &root, Some(&active));
                        chat_writer::bump_projection_version(&mut txn, &root);
                    }
                    EventBody::MessageDelta { .. } | EventBody::ReasoningDelta { .. } => {
                        // §7.2 状态推进：内容增量到达 → accepting → running
                        // （参考实现：首条内容增量即 turn 开始运行）。回放
                        // 模式跳过（回放 turn 由 EndLoadReplay 终态化）。
                        if !replay_active
                            && chat_writer::set_active_turn_status_if(
                                &mut txn,
                                &root,
                                "accepting",
                                "running",
                            )
                        {
                            chat_writer::bump_projection_version(&mut txn, &root);
                        }
                    }
                    EventBody::ToolCallStarted { .. }
                    | EventBody::ToolCallUpdated { .. }
                    | EventBody::ToolCallCompleted { .. }
                    | EventBody::PermissionResolved { .. }
                    | EventBody::PermissionExpired { .. } => {
                        // 纯 chat 事件或已在外层单独处理（CAS）：无 control
                        // 写入。
                    }
                }
            }
        }
    }
}

fn default_tool_call() -> ToolCallProjection {
    ToolCallProjection {
        tool_call_id: String::new(),
        turn_id: String::new(),
        name: String::new(),
        status: ToolCallStatus::Pending,
        arguments: None,
        result: None,
        public_error: None,
        permission_id: None,
    }
}

// ---------------------------------------------------------------------------
// control 侧写入辅助
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)] // 与 doc_manager 提交面一致：字段摊开的写入原语
fn write_permission_request(
    txn: &mut TransactionCtx<'_>,
    root: &yrs::MapRef,
    permission_id: &str,
    turn_id: &str,
    tool_call_id: Option<&str>,
    title: &str,
    description: Option<&str>,
    options: &[PermissionOptions],
    expires_at: &str,
) {
    let perms = root.get_or_init::<_, yrs::MapRef>(txn, "pending_permissions");
    let pm = perms.get_or_init::<_, yrs::MapRef>(txn, permission_id);
    pm.insert(txn, "permission_id", permission_id.to_string());
    pm.insert(txn, "turn_id", turn_id.to_string());
    match tool_call_id {
        Some(t) => pm.insert(txn, "tool_call_id", t.to_string()),
        None => pm.insert(txn, "tool_call_id", yrs::Any::Null),
    };
    pm.insert(txn, "title", title.to_string());
    match description {
        Some(d) => pm.insert(txn, "description", d.to_string()),
        None => pm.insert(txn, "description", yrs::Any::Null),
    };
    let opts = pm.get_or_init::<_, yrs::ArrayRef>(txn, "options");
    for o in options {
        opts.push_back(txn, crate::state::permission::option_str(*o).to_string());
    }
    pm.insert(
        txn,
        "status",
        crate::state::permission::permission_status_str(PermissionStatus::Pending),
    );
    pm.insert(txn, "expires_at", expires_at.to_string());
    pm.insert(txn, "decision", yrs::Any::Null);
}

fn write_agent_status(
    txn: &mut TransactionCtx<'_>,
    root: &yrs::MapRef,
    status: &str,
    public_error: Option<&PublicError>,
) {
    let agent = root.get_or_init::<_, yrs::MapRef>(txn, "agent");
    agent.insert(txn, "status", status.to_string());
    match public_error {
        Some(e) => write_public_error(txn, &agent, e),
        None => {
            agent.insert(txn, "public_error", yrs::Any::Null);
        }
    };
}

fn write_capabilities(txn: &mut TransactionCtx<'_>, root: &yrs::MapRef, caps: &[String]) {
    let agent = root.get_or_init::<_, yrs::MapRef>(txn, "agent");
    let arr = agent.get_or_init::<_, yrs::ArrayRef>(txn, "capabilities");
    // 全量覆盖（§6.3「覆盖当前状态」）：先清后写。
    let len = arr.len(txn);
    for i in (0..len).rev() {
        arr.remove(txn, i);
    }
    for c in caps {
        arr.push_back(txn, c.clone());
    }
}

fn write_chat_info(
    txn: &mut TransactionCtx<'_>,
    root: &yrs::MapRef,
    title: Option<&str>,
    status: Option<ChatStatus>,
    active_turn_id: Option<&str>,
) {
    // Session Doc 会话面（对齐 Chat/Session 双 Doc）：写入根级 `session` map。
    let sm = root.get_or_init::<_, yrs::MapRef>(txn, "session");
    if let Some(t) = title {
        sm.insert(txn, "title", t.to_string());
    }
    if let Some(s) = status {
        sm.insert(txn, "status", chat_status_str(s));
    }
    if let Some(a) = active_turn_id {
        sm.insert(txn, "active_turn_id", a.to_string());
    }
}

fn write_public_error(txn: &mut TransactionCtx<'_>, map: &yrs::MapRef, e: &PublicError) {
    let em = map.insert(txn, "public_error", yrs::MapPrelim::default());
    em.insert(txn, "code", e.code.clone());
    em.insert(txn, "message", e.message.clone());
}

pub(crate) fn chat_status_str(s: ChatStatus) -> &'static str {
    match s {
        ChatStatus::Accepting => "accepting",
        ChatStatus::Active => "active",
        ChatStatus::Ended => "ended",
        ChatStatus::Closed => "closed",
        ChatStatus::Crashed => "crashed",
    }
}

fn chat_status_from_str(s: &str) -> Option<ChatStatus> {
    match s {
        "accepting" => Some(ChatStatus::Accepting),
        "active" => Some(ChatStatus::Active),
        "ended" => Some(ChatStatus::Ended),
        "closed" => Some(ChatStatus::Closed),
        "crashed" => Some(ChatStatus::Crashed),
        _ => None,
    }
}

fn turn_status_from_str(s: &str) -> TurnStatus {
    match s {
        "accepting" => TurnStatus::Accepting,
        "running" => TurnStatus::Running,
        "awaitingPermission" => TurnStatus::AwaitingPermission,
        "cancelling" => TurnStatus::Cancelling,
        "completed" => TurnStatus::Completed,
        "cancelled" => TurnStatus::Cancelled,
        "interrupted" => TurnStatus::Interrupted,
        "failed" => TurnStatus::Failed,
        _ => TurnStatus::Accepting,
    }
}
