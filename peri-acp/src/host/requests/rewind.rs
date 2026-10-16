//! Session rewind 命令 handler：rewind-candidates / rewind-preview / rewind
//! 与 rewind cap 校验（自 requests.rs 拆出，请求分发见 `host/requests.rs`）。
//!
//! **历史事实源是存储，不是 Host 内存快照**：Full Compact 会把退出有效上下文的原文
//! 归档（`excluded`），热态只保留有效集合，用热态回答「目标是否存在」会把归档目标
//! 判成不存在。候选、预览与执行一律在存储的完整历史上定位目标（REQ-T04 明确允许
//! 显式历史操作读取归档正文）；Host 快照只在提交成功后按同一结果重建，仍是展示投影。
//!
//! **破坏历史范围的操作推进结构版本**（REQ-T04 末段、验收 A19「旧 cursor 失效」）：
//! durable 提交成功一次，本会话的 `SessionState::structure_version` 就 `+1`（**恰好一次**，
//! 与提交共用同一个成功分支；失败分支不碰它——内存不得领先于已提交事实）。已发出的
//! 历史分页游标在**进入存储之前**按这个版本被拒（`HistoryCursorEnvelope`）。
//! 精确删除（`DurableRewindCommit::RemovedIds`，slash `/rewind` 专用）同样破坏范围，
//! 但它与 RPC 路径共用本模块的提交点语义；当前 checkout 中 slash 执行入口没有生产调用者
//! （见 `.recon/R4-verification.md` §I1 CAVEAT），接入时必须走同一处递增。
//!
//! **「提交没生效」不是成功**（F1）：存储的未知边界语义是「什么都不做」，而本 handler 在
//! 提交返回后会把热态改写成「已截断 + 已清零」的那份。两条路径一旦分叉，冷恢复与运行中的
//! 上下文就说的不是同一件事。因此 [`execute_rewind`] 把「边界已不在存储里」作为
//! **失败**（[`RewindDurability::BoundaryMissing`]）返回：不发布热态、不递增结构版本，
//! RPC 错误码 `-32013` + `data.reason = "rewind_boundary_missing"`
//! （码位选取与登记见 [`crate::dispatch::session_load`] 的模块文档）。
//!
//! **事件载荷 = 发布的有效视图**（F4）：`RewindCompleted` 携带的保留前缀由本 handler 按
//! 同一边界计划算出（[`RewindPlan::effective_payloads`]），提交成功后发布到热态的是同一份。
//! 事件若退回 canonical 前缀（含仍被存活摘要覆盖的正文），TUI 会以它重建出模型与存储都不
//! 认为有效的条目。
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use peri_acp_types::messages::{BaseMessage, MessageId};
use peri_acp_types::session_resources::{RewindBoundary, SessionSnapshot};
use peri_acp_types::store::{history, MessageFlags, PersistedPayload};
use peri_acp_types::thread::ThreadId;
use peri_acp_types::PeriCaps;
use serde_json::{json, Value};

use super::super::{AcpServerConfig, SessionState};
use crate::session::command::DurableRewindCommit;
use crate::{dispatch, transport::types::AcpError};

/// 候选/预览/执行共用的会话存在性检查（wire 错误形态与既有请求一致）。
fn require_registered_session(
    sessions: &HashMap<String, SessionState>,
    session_id: &str,
) -> Result<(), AcpError> {
    if sessions.contains_key(session_id) {
        Ok(())
    } else {
        Err(AcpError::new(-32602, "session not found"))
    }
}

pub(super) async fn handle_rewind_candidates(
    params: &Value,
    cfg: &AcpServerConfig,
    sessions: &mut HashMap<String, SessionState>,
) -> Result<Value, AcpError> {
    let session_id = params
        .get("sessionId")
        .or_else(|| params.get("session_id"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| AcpError::new(-32602, "missing sessionId"))?;
    require_rewind_cap(&cfg.session_manager.get_caps(session_id))?;
    require_registered_session(sessions, session_id)?;
    // 候选来自存储的完整历史：被 Full 归档的 user 消息仍是合法回退目标。
    let history = dispatch::load_canonical_session_payloads(&cfg.controller, session_id).await?;
    dispatch::rewind_persisted_candidates(&history)
}

pub(super) async fn handle_rewind_preview(
    params: &Value,
    cfg: &AcpServerConfig,
    sessions: &mut HashMap<String, SessionState>,
) -> Result<Value, AcpError> {
    let session_id = params
        .get("sessionId")
        .or_else(|| params.get("session_id"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| AcpError::new(-32602, "missing sessionId"))?
        .to_string();
    require_rewind_cap(&cfg.session_manager.get_caps(&session_id))?;
    require_registered_session(sessions, &session_id)?;
    let cwd = sessions
        .get(&session_id)
        .map(|s| s.cwd.clone())
        .ok_or_else(|| AcpError::new(-32602, "session not found"))?;
    // 预览与执行必须看同一份历史：同一来源（存储完整历史）才能让 fingerprint 有意义。
    let history = dispatch::load_canonical_session_payloads(&cfg.controller, &session_id).await?;
    let history = message_projection(&history);
    // Phase 5 Step 5：RewindError 变体删除，preview 为只读路径
    // 零事件——不再需要 event_sink。
    dispatch::rewind_preview(params, &history, &cwd, &session_id).await
}

/// 提交成功后重建 Host 热态：把**已经算好的**有效视图装进内存容器，并同步派生 `history`
/// 投影。被移除的 id 不留在任何容器里——热态必须等于冷恢复（`load_active_snapshot`）
/// 会看到的那一份，否则下一次 prompt 会把已删除的历史带回执行上下文。
///
/// 有效视图由调用方（[`RewindPlan::effective_payloads`]）算一次并共用，事件载荷与 wire
/// `history` 都是它的投影：三处若各算一次，就会再次出现「事件与热态不同源」的分叉。
///
/// 失败路径不调用本函数（与 Compact 的提交契约一致：durable 失败不得改内存）。
fn publish_rewind_view(state: &mut SessionState, effective: Vec<PersistedPayload>) {
    state.history = message_projection(&effective);
    state.history_payloads = effective;
}

/// 保留前缀的有效视图（纯计算）：清零集合里的 id 恢复有效，其余仍被排除的条目不在其中。
fn effective_view(
    retained: &[PersistedPayload],
    clear_excluded: &[MessageId],
    flags: &HashMap<MessageId, MessageFlags>,
) -> Vec<PersistedPayload> {
    let cleared: HashSet<&MessageId> = clear_excluded.iter().collect();
    retained
        .iter()
        .filter(|payload| {
            let still_excluded = flags.get(&payload.id()).is_some_and(|flags| flags.excluded)
                && !cleared.contains(&payload.id());
            !still_excluded
        })
        .cloned()
        .collect()
}

/// payload → BaseMessage 投影（reminder 等无消息形态的条目不在其中）。
fn message_projection(payloads: &[PersistedPayload]) -> Vec<BaseMessage> {
    payloads
        .iter()
        .filter_map(|payload| payload.as_message().cloned())
        .collect()
}

/// rewind 的权威读取：完整逻辑上下文（继承区在前、自有 payload 在后，**含被 Full
/// 归档的正文**）与自有区在其中的起点、自有区 flags，全部来自**同一次一致读取**。
///
/// 为什么不是 `load_session_history`：那条读取只给 payload，既给不出自有区起点
/// （继承区长度），也给不出 flags——两者都是 `rewind_clear_excluded_ids` 与热态重建的
/// 输入。分两次读取则清零依据与边界可能来自两份不同的历史，因此这里取同一次快照。
struct CanonicalRewind {
    payloads: Vec<PersistedPayload>,
    /// 自有区起点 = 继承区长度（继承条目不属于本会话，不是回退边界）。
    own_start: usize,
    flags: HashMap<MessageId, MessageFlags>,
}

impl CanonicalRewind {
    async fn load(cfg: &AcpServerConfig, session_id: &str) -> Result<Self, AcpError> {
        let SessionSnapshot {
            payloads: own_payloads,
            flags,
            inherited,
            ..
        } = cfg
            .controller
            .sessions()
            .load_session_snapshot(&ThreadId::from(session_id.to_string()))
            .await
            .map_err(|error| {
                AcpError::new(-32603, format!("session history load failed: {error}"))
            })?;
        // 完整逻辑上下文的顺序 = 继承区在前、自有 payload 在后（与 load_session_history 一致）。
        let own_start = inherited.payloads.len();
        let mut payloads = inherited.payloads;
        payloads.extend(own_payloads);
        Ok(Self {
            payloads,
            own_start,
            flags,
        })
    }

    /// 在完整历史上定位目标并算出边界计划。
    ///
    /// 目标必须落在自有区：继承条目不属于本会话，存储的边界提交找不到它（未知边界 =
    /// 无变更），在这里拒绝比留下「内存已截断、存储没动」的分叉安全。两种拒绝共用既有
    /// 的「未找到目标」报文，wire 文本与内存时代保持一致。
    fn plan(&self, target_message_id: &str) -> Result<RewindPlan, AcpError> {
        let not_found = || {
            AcpError::new(
                -32602,
                format!("rewind: 未找到目标消息 {target_message_id}"),
            )
        };
        let target_index = self
            .payloads
            .iter()
            .position(|payload| payload.id().as_uuid().to_string() == target_message_id)
            .ok_or_else(not_found)?;
        if target_index < self.own_start {
            return Err(not_found());
        }
        let ids: Vec<MessageId> = self.payloads.iter().map(PersistedPayload::id).collect();
        // 用户 rewind 移除目标本身及以后（与 transcript 的 KeepThrough 语义不同）。
        let boundary = RewindBoundary::RemoveFrom(ids[target_index]);
        let keep_len = history::rewind_keep_len(&ids, boundary)
            .map_err(|_| AcpError::new(-32603, "rewind target missing from canonical history"))?;
        // 清零依据只取**自有区**的保留前缀（继承条目永不参与排除，交给规则会终结领导段）。
        let own_kept = &self.payloads[self.own_start..keep_len];
        let entries: Vec<history::RewindFlagEntry> = own_kept
            .iter()
            .map(|payload| {
                (
                    payload.id(),
                    self.flags.get(&payload.id()).cloned().unwrap_or_default(),
                    matches!(payload.as_message(), Some(BaseMessage::System { .. })),
                )
            })
            .collect();
        Ok(RewindPlan {
            boundary,
            keep_len,
            clear_excluded: history::rewind_clear_excluded_ids(&entries, entries.len()),
        })
    }
}

/// rewind 边界计划：存储提交与热态重建共用同一份输入（一次计算，两处使用）。
struct RewindPlan {
    boundary: RewindBoundary,
    /// 保留前缀长度（完整历史口径）。
    keep_len: usize,
    /// 保留前缀里必须清零 `excluded` 的 id（清零后这些原文重新生效）。
    clear_excluded: Vec<MessageId>,
}

impl RewindPlan {
    /// 保留前缀的**有效视图**（payload 口径）：本计划的唯一重建结果，提交成功后的热态、
    /// `RewindCompleted` 事件载荷与 wire `history` 都取这一份。
    fn effective_payloads(&self, canonical: &CanonicalRewind) -> Vec<PersistedPayload> {
        effective_view(
            &canonical.payloads[..self.keep_len],
            &self.clear_excluded,
            &canonical.flags,
        )
    }
}

fn require_rewind_cap(caps: &PeriCaps) -> Result<(), AcpError> {
    if caps.rewind {
        Ok(())
    } else {
        Err(AcpError::new(
            -32601,
            "peri.rewind capability not negotiated",
        ))
    }
}
pub(super) async fn handle_rewind(
    params: &Value,
    cfg: &AcpServerConfig,
    sessions: &mut HashMap<String, SessionState>,
    transport: &Arc<dyn crate::transport::AcpTransport>,
) -> Result<Value, AcpError> {
    let session_id = params
        .get("sessionId")
        .or_else(|| params.get("session_id"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| AcpError::new(-32602, "missing sessionId"))?
        .to_string();
    require_rewind_cap(&cfg.session_manager.get_caps(&session_id))?;
    let (cwd, workspace_pool) = {
        let s = sessions
            .get(&session_id)
            .ok_or_else(|| AcpError::new(-32602, "session not found"))?;
        if s.closing {
            return Err(AcpError::new(-32010, "Session is closing"));
        }
        let local = s
            .environment
            .as_ref()
            .map(|environment| &environment.cfg)
            .unwrap_or(cfg);
        (s.cwd.clone(), local.mcp_pool.clone())
    };
    let target_message_id = params
        .get("target_message_id")
        .and_then(Value::as_str)
        .ok_or_else(|| AcpError::new(-32602, "missing target_message_id"))?;
    // 存储是事实源：目标定位、保留长度与清零集合来自同一次一致读取。
    let canonical = CanonicalRewind::load(cfg, &session_id).await?;
    let plan = canonical.plan(target_message_id)?;
    let history = message_projection(&canonical.payloads);
    // 保留前缀的**有效视图**只算一次，三处共用：事件载荷、发布到热态、wire 的 `history`
    // 字段。事件若用 canonical 前缀（含仍被存活摘要覆盖的正文），TUI 会以它重建出模型与
    // 存储都不认为有效的 transcript（F4）。
    let effective = plan.effective_payloads(&canonical);
    let effective_history = message_projection(&effective);
    let event_sink: Arc<dyn crate::session::event_sink::EventSink> =
        Arc::new(crate::session::event_sink::TransportEventSink::new(
            transport.clone(), // transport: &Arc<dyn AcpTransport>（签名改动见下方实现注记）
            cfg.session_manager.caps_registry(),
        ));
    let peri_config_snapshot = Arc::new(cfg.peri_config.read().clone());
    let response = dispatch::rewind_execute(
        params,
        history,
        &cwd,
        &peri_config_snapshot,
        &event_sink,
        None, // auxiliary_model：RewindCommand 不使用
        &tokio_util::sync::CancellationToken::new(),
        cfg.controller.as_ref(),
        Some(session_id.clone()),
        None, // bg_event_tx
        None, // task_manager
        None,
        None,
        None,
        None, // frozen_*：RewindCommand 不使用
        workspace_pool,
        // 删除与清零在同一次存储提交里生效；执行体只在文件复原成功之后、事件之前
        // 发出这一次调用（失败即整体失败，热态不动）。
        DurableRewindCommit::Boundary {
            boundary: plan.boundary,
            clear_excluded: plan.clear_excluded.clone(),
            effective_history,
        },
    )
    .await?;
    // 提交成功之后才重建热态；失败路径已在上面返回（含「边界已不在存储里」的
    // `BoundaryMissing`：存储一行未动，热态不得前进，结构版本也不得递增）。
    let mut response = response;
    if let Some(state) = sessions.get_mut(&session_id) {
        // 结构版本与热态重建在同一个成功分支里推进：范围已变，已发出的分页游标作废。
        // 这里**只能递增一次**（本函数是 RPC rewind 的唯一成功出口）。
        state.structure_version = state.structure_version.saturating_add(1);
        publish_rewind_view(state, effective);
        // wire 里的 `history` 与热态投影共用同一份重建结果：执行体看到的是含归档正文的
        // 完整历史，直接把它当结果返回会把已归档的正文重新暴露给调用方。
        response["history"] = json!(&state.history);
    }
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use peri_acp_types::messages::BaseMessage;
    use peri_acp_types::store::PersistedPayload;
    use peri_acp_types::system_reminder::{
        ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
        ReminderSource, SystemReminder, TrustedSystemReminderFactory, SYSTEM_REMINDER_VERSION,
    };

    fn reminder(id: peri_acp_types::messages::MessageId) -> PersistedPayload {
        let reminder = TrustedSystemReminderFactory::for_producer()
            .construct(SystemReminder {
                version: SYSTEM_REMINDER_VERSION,
                category: ReminderCategory::Task,
                source: ReminderSource("rewind_test".into()),
                kind: "interleave".into(),
                severity: ReminderSeverity::Info,
                delivery: ReminderDelivery::Configurable,
                audiences: ReminderAudiences(vec![ReminderAudience::Model]),
                body: "reminder".into(),
                summary: None,
                metadata: serde_json::json!({}),
            })
            .unwrap();
        PersistedPayload::SystemReminder { id, reminder }
    }

    #[test]
    fn effective_view_keeps_interleaved_reminder_and_derives_projection() {
        let user1 = BaseMessage::human("user1");
        let ai1 = BaseMessage::ai("ai1");
        let user2 = BaseMessage::human("user2");
        let reminder_id = peri_acp_types::messages::MessageId::new();
        let retained = vec![
            PersistedPayload::Message(user1.clone()),
            reminder(reminder_id),
            PersistedPayload::Message(ai1.clone()),
            PersistedPayload::Message(user2.clone()),
        ];

        let payloads = effective_view(&retained, &[], &HashMap::new());

        assert_eq!(
            payloads
                .iter()
                .map(PersistedPayload::id)
                .collect::<Vec<_>>(),
            vec![user1.id(), reminder_id, ai1.id(), user2.id()]
        );
        assert_eq!(
            message_projection(&payloads)
                .iter()
                .map(BaseMessage::id)
                .collect::<Vec<_>>(),
            vec![user1.id(), ai1.id(), user2.id()]
        );
    }

    #[test]
    fn effective_view_drops_still_excluded_and_restores_cleared_bodies() {
        let covered = BaseMessage::human("covered by the removed summary");
        let summary = BaseMessage::human("summary that the rewind deletes");
        let retained = [
            PersistedPayload::Message(covered.clone()),
            PersistedPayload::Message(summary.clone()),
        ];
        let excluded = MessageFlags {
            excluded: true,
            ..Default::default()
        };
        let flags = HashMap::from([(covered.id(), excluded.clone())]);

        // 摘要被删 → 清零依据消失：原文重新有效。
        let restored = effective_view(&retained[..1], &[covered.id()], &flags);
        assert_eq!(
            restored
                .iter()
                .map(PersistedPayload::id)
                .collect::<Vec<_>>(),
            vec![covered.id()]
        );

        // 摘要仍在 → 保留前缀的排除依旧成立：原文不进有效视图。
        let still_archived = effective_view(&retained[..1], &[], &flags);
        assert!(still_archived.is_empty());
        // 投影只认消息形态：reminder 不会凭空出现在 history 里。
        assert!(message_projection(&[reminder(MessageId::new())]).is_empty());
    }

    /// 继承区在保留前缀里：它不是本会话的条目，既不在 `flags` 里也不在清零集合里，
    /// 因此 `effective_view` 必须原样保留——父会话的 rewind 不改写子会话的继承前缀
    /// （REQ-T04、不变量 4）。
    #[test]
    fn effective_view_keeps_the_inherited_prefix_untouched() {
        let inherited = BaseMessage::human("inherited from the parent session");
        let archived_body = BaseMessage::human("own body still covered by the summary");
        let summary = BaseMessage::ai("summary removed by the rewind");
        let retained = [
            PersistedPayload::Message(inherited.clone()),
            PersistedPayload::Message(archived_body.clone()),
            PersistedPayload::Message(summary),
        ];
        let flags = HashMap::from([(
            archived_body.id(),
            MessageFlags {
                excluded: true,
                ..Default::default()
            },
        )]);

        // 摘要被删 → 清零依据消失：自有原文重新有效，继承前缀原样在前。
        let restored = effective_view(&retained[..2], &[archived_body.id()], &flags);
        assert_eq!(
            restored
                .iter()
                .map(PersistedPayload::id)
                .collect::<Vec<_>>(),
            vec![inherited.id(), archived_body.id()]
        );

        // 摘要仍活着（保留前缀不含它）：继承前缀保留，被覆盖的原文继续归档。
        let still_archived = effective_view(&retained[..2], &[], &flags);
        assert_eq!(
            still_archived
                .iter()
                .map(PersistedPayload::id)
                .collect::<Vec<_>>(),
            vec![inherited.id()]
        );
    }

    #[test]
    fn plan_rejects_targets_outside_the_own_region() {
        let inherited = BaseMessage::human("inherited");
        let own = BaseMessage::human("own");
        let canonical = CanonicalRewind {
            payloads: vec![
                PersistedPayload::Message(inherited.clone()),
                PersistedPayload::Message(own.clone()),
            ],
            own_start: 1,
            flags: HashMap::new(),
        };

        let error = canonical
            .plan(&inherited.id().as_uuid().to_string())
            .err()
            .expect("继承区的目标不属于本会话自己的历史，必须拒绝");
        assert_eq!(error.code, -32602);
        let plan = canonical.plan(&own.id().as_uuid().to_string()).unwrap();
        assert_eq!(plan.keep_len, 1, "RemoveFrom 移除目标本身及以后");
        assert!(plan.clear_excluded.is_empty());
        assert!(canonical
            .plan("00000000-0000-0000-0000-000000000000")
            .is_err());
    }
}
