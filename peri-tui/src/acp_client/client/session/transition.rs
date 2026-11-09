//! New-session and load transitions under the operation gate.

use peri_acp::transport::{AcpTransport, types::AcpError};
use serde_json::json;

use crate::acp_client::client::{AcpTuiClient, ClientProjectionMode, SessionLoadReservation};
use crate::acp_client::interaction_lifecycle::{TransitionKind, TransitionOperation};

use super::guards::{PreparingSessionGuard, SessionProjectionGuard};

/// initial 失败时收敛投影。
///
/// 同会话的旧窗口仍然有效：先把它在 initial 期间暂存的旧流补发按 seq 并入窗口，
/// 再把恢复后的状态重新送入桥接视图（`notify_view` 只读状态，不修改交付身份）。
/// 恢复期间继续持有暂存槽：新通知只能在状态发布与重放派发完成后按恢复后的高水位
/// 接纳，不能越过缓存先渲染。视图、重放与之后的实时通知共用 `LOCAL_EVENT_TX`
/// 这一条 FIFO，由唯一 forwarder 串行写入 bridge，因此暂存槽必须在重放入队之后
/// 才释放。跨会话失败不恢复旧视图；没有可恢复窗口时丢弃暂存并退回空边界。
fn settle_failed_initial_projection(
    projection: &mut SessionProjectionGuard<'_>,
    previous_v2: &crate::kit::history_v2::HistoryV2State,
    session_id: &str,
) -> bool {
    let restore = previous_v2.is_ready() && previous_v2.session_id.as_deref() == Some(session_id);
    if restore {
        let mut restored = previous_v2.clone();
        let (events, replay) = crate::kit::acp_notifier::replay_pending_into_guarded(&mut restored);
        // 恢复后的高水位必须写回 atom，失败后的实时事件才能继续按连续序号接纳。
        crate::kit::history_v2::restore_published(restored.clone());
        if projection.client.projection_mode == ClientProjectionMode::Interactive {
            // 明确恢复边界为同一目标会话；不递增 reset，桥接任务会先应用
            // load 起点的 reset，再按 active_session_id 接收这条重投影事件。
            crate::kit::atoms::ACTIVE_SESSION_ID.set(session_id.to_owned());
            crate::kit::history_v2::consumer::notify_view(&restored);
            crate::kit::acp_notifier::dispatch_local_events(events);
        }
        // 状态、视图与缓存重放都已入队：释放暂存槽，后续通知按新水位进入同一
        // FIFO 的尾部，不会越序插到重放事件之前。
        if let Some(replay) = replay {
            replay.finish();
        }
    } else {
        crate::kit::history_v2::live::clear_pending(session_id);
        if projection.client.projection_mode == ClientProjectionMode::Interactive {
            crate::kit::session_boundary::project_session_boundary(None);
            crate::kit::history_v2::consumer::reset_consumer_state();
        }
    }
    projection.retain_projection |= restore;
    restore
}

impl AcpTuiClient {
    /// initial 失败的统一收敛：可恢复旧窗口时把通知路由交还旧会话。
    ///
    /// 路由必须先于恢复视图/暂存重放生效：服务端错误响应之后会立刻把候选期更新
    /// 补回旧流，pump 需要按旧会话身份继续放行这些通知。
    fn fail_initial_transition(&self, generation: u64, session_id: &str, restore_route: bool) {
        if restore_route {
            self.lifecycle
                .fail_transition_restoring_route(generation, session_id);
        } else {
            self.lifecycle.fail_transition(generation);
        }
    }

    /// Create a new agent session.
    ///
    /// Closes the previous session (if any) to release its history, AgentPool,
    /// and FrozenSessionData from the server-side sessions HashMap.
    ///
    /// 整段建立过程都对用户可见（`SESSION_PREPARING`）：这段窗口里输入既不在待发送
    /// 队列、也还没有会话，等待中的输入可能来自提交，也可能来自启动时的那一次。
    pub async fn new_session(&self, cwd: &str, model: Option<&str>) -> Result<String, AcpError> {
        // 用户显式换会话：排队中的输入从此不能再落到新建的会话上，必须先于
        // operation gate 可见（等待中的 cohort 在持锁判定时会读到这次决定）。
        self.bump_session_switch_seq();
        let _operation = self.lifecycle.operation_gate().lock().await;
        self.new_session_under_gate(cwd, model, TransitionOperation::New)
            .await
    }

    pub(super) async fn new_session_under_gate(
        &self,
        cwd: &str,
        model: Option<&str>,
        operation: TransitionOperation,
    ) -> Result<String, AcpError> {
        let _preparing = PreparingSessionGuard::enter();
        *self.restore_error.lock().unwrap() = None;
        self.clear_startup_restore_target();
        if self.history_v2_active() {
            // 切换会话前 best-effort 关闭旧窗口；关闭确认属于服务端，本地只复位状态。
            crate::kit::history_v2::consumer::request_close_current();
            crate::kit::history_v2::reset_state();
        }
        self.project_execution_cwd(None);
        let start = self
            .lifecycle
            .begin_transition_with_operation(TransitionKind::New, None, operation)
            .map_err(|message| AcpError::new(-32603, message))?;
        let transition = self.lifecycle.arm_transition(start.generation);
        let mut projection = SessionProjectionGuard {
            client: self,
            committed: false,
            retain_projection: false,
        };
        if self.projection_mode == ClientProjectionMode::Interactive {
            crate::kit::session_boundary::project_session_boundary(None);
        }
        self.settle_claims_owned(start.claims).await;
        // 上一次关闭失败留下的旧身份优先：`fail_transition` 之后 lifecycle 不再保留
        // 它，但服务端可能仍在 Closing，绕过它就会把旧会话与用户意图一起丢掉。
        let close_target = self.pending_close_target().or(start.from);
        if let Some(ref old_sid) = close_target {
            if let Err(error) = self.close_previous_session(old_sid).await {
                self.lifecycle.fail_transition(start.generation);
                transition.disarm();
                if self.projection_mode == ClientProjectionMode::Interactive {
                    crate::kit::session_boundary::project_session_boundary(None);
                }
                // 旧会话清理失败不写入粘滞 `restore_error`：那会让后续每次
                // 建立/发送都被同一条陈旧错误拦截且无从清除。改记待关闭身份，
                // 下一次建立会话先重放同一关闭。
                *self.restore_error.lock().unwrap() = None;
                self.remember_close_failure(old_sid, None, &error);
                tracing::warn!(
                    session_id = %old_sid,
                    %error,
                    "previous session close incomplete; kept pending for retry"
                );
                return Err(error);
            }
            self.clear_pending_close();
        }

        let params = json!({ "cwd": cwd, "model": model });
        let result = match self.transport.send_request("session/new", params).await {
            Ok(result) => result,
            Err(error) => {
                self.lifecycle.fail_transition(start.generation);
                transition.disarm();
                if self.projection_mode == ClientProjectionMode::Interactive {
                    crate::kit::session_boundary::project_session_boundary(None);
                }
                return Err(error);
            }
        };
        // ACP protocol uses camelCase: {"sessionId": "..."}
        let session_id = result
            .get("sessionId")
            .or_else(|| result.get("session_id"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| AcpError::new(-32603, "no session_id in response"))?
            .to_string();
        let effective_cwd = result
            .pointer("/_meta/peri.sessionWorkspaceV1/workspace/cwd")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(cwd)
            .to_string();
        #[cfg(test)]
        self.pause_before_transition_commit().await;
        if self.projection_mode == ClientProjectionMode::Interactive {
            crate::kit::session_boundary::project_session_boundary(Some(&session_id));
        }
        self.lifecycle
            .commit_stable(start.generation, session_id.clone());
        self.finish_session_initialization(start.generation, &session_id)
            .await?;
        // S5.4：新建与恢复统一建立窗口。服务端只向持有 window ticket 的连接发送
        // `peri/history_committed`；新会话不 open 会让实时段永远停在 position=None。
        // H5b：V2 连接的新会话没有 load initial，用 `_peri/history open latest` 建立首窗。
        if self.history_v2_active() {
            crate::kit::history_v2::consumer::request_open_latest(&session_id);
        } else {
            crate::kit::history_page_consumer::enqueue_window_open(
                &session_id,
                crate::kit::history_paging::WindowDirection::Before,
            );
        }
        self.project_execution_cwd(Some(effective_cwd));
        *self.restore_error.lock().unwrap() = None;
        projection.committed = true;
        transition.disarm();
        self.flush_buffered();
        if self.projection_mode == ClientProjectionMode::Interactive {
            crate::kit::acp_events::request_bg_task_snapshot();
        }
        Ok(session_id)
    }

    /// Load an existing session from ThreadStore history.
    /// Used when restoring a historical thread so the ACP server has the full context.
    ///
    /// Closes the previous session (if any) to release server-side memory.
    pub async fn load_session(
        &self,
        session_id: &str,
        cwd: &str,
        model: Option<&str>,
    ) -> Result<String, AcpError> {
        let _operation = self.lifecycle.operation_gate().lock().await;
        // 交互式切换是新的用户意图：不再自动重试上一次启动恢复目标。
        self.clear_startup_restore_target();
        self.load_session_under_gate(session_id, cwd, model, None, TransitionOperation::New)
            .await
    }

    /// 线程切换消费者在预约保护下加载。
    ///
    /// 预约身份在认领与路由切换的同一临界区内生效：准入快照要么看到
    /// 「未认领 + 旧路由」，要么看到「已认领 + Transitioning」，不会把本次
    /// load 之后的输入绑定到旧会话。
    pub(crate) async fn load_reserved_session(
        &self,
        reservation: &SessionLoadReservation,
        session_id: &str,
        cwd: &str,
        model: Option<&str>,
    ) -> Result<String, AcpError> {
        let _operation = self.lifecycle.operation_gate().lock().await;
        // 交互式切换是新的用户意图：不再自动重试上一次启动恢复目标。
        self.clear_startup_restore_target();
        self.load_session_under_gate(
            session_id,
            cwd,
            model,
            Some(reservation),
            TransitionOperation::Owned(reservation.operation()),
        )
        .await
    }

    pub(super) async fn load_session_under_gate(
        &self,
        session_id: &str,
        cwd: &str,
        model: Option<&str>,
        reservation: Option<&SessionLoadReservation>,
        operation: TransitionOperation,
    ) -> Result<String, AcpError> {
        *self.restore_error.lock().unwrap() =
            Some("session restore has not completed; retry or create a new session".into());
        let effective_cwd = if self.supports_session_workspace() {
            match self.session_context(Some(session_id), None).await {
                Ok(context) => context
                    .workspace
                    .cwd
                    .into_os_string()
                    .into_string()
                    .map_err(|_| AcpError::new(-32603, "session cwd is not valid UTF-8"))?,
                Err(error) => {
                    *self.restore_error.lock().unwrap() = Some(error.to_string());
                    return Err(error);
                }
            }
        } else {
            cwd.to_string()
        };
        self.project_execution_cwd(None);
        let start = self
            .claim_and_begin_transition(
                reservation,
                TransitionKind::Load,
                Some(session_id.to_string()),
                operation,
            )
            .map_err(|message| AcpError::new(-32603, message))?;
        let transition = self.lifecycle.arm_transition(start.generation);
        // initial 是窗口替换的候选：候选成功前不撤销旧窗口，也不关闭它的服务端
        // 票据（否则失败后快照虽在，page/body 仍会 `window_closed`）。同会话且已有
        // 可恢复窗口时，暂存登记与快照在同一接纳临界区完成：旧流更新要么已落在
        // 快照里，要么进入暂存等待失败恢复。
        let history_v2 = self.history_v2_active();
        let previous = crate::kit::history_v2::snapshot();
        let mut retain_previous =
            history_v2 && previous.is_ready() && previous.session_id.as_deref() == Some(session_id);
        let previous_v2 = if retain_previous {
            let current = crate::kit::history_v2::live::begin_pending_snapshot(session_id);
            // 临界区内复核：快照与暂存登记必须对应同一个可恢复窗口。
            retain_previous =
                current.is_ready() && current.session_id.as_deref() == Some(session_id);
            current
        } else {
            previous
        };
        let mut projection = SessionProjectionGuard {
            client: self,
            committed: false,
            retain_projection: false,
        };
        if self.projection_mode == ClientProjectionMode::Interactive {
            if retain_previous {
                crate::kit::session_boundary::project_session_boundary_retaining_history(Some(
                    session_id,
                ));
            } else {
                crate::kit::session_boundary::project_session_boundary(Some(session_id));
            }
        }
        self.settle_claims_owned(start.claims).await;
        let close_target = self
            .pending_close_target()
            .or(start.from)
            .filter(|candidate| *candidate != session_id);
        if let Some(ref old_sid) = close_target {
            if let Err(error) = self.close_previous_session(old_sid).await {
                self.lifecycle.fail_transition(start.generation);
                transition.disarm();
                if self.projection_mode == ClientProjectionMode::Interactive {
                    crate::kit::session_boundary::project_session_boundary(None);
                }
                // 与 `new_session_under_gate` 同一口径：清理旧会话失败不是
                // 恢复失败，不写入粘滞 `restore_error`（含本函数入口的占位）。
                // 额外记住本次要加载的目标，避免用户的历史意图被换成空会话。
                *self.restore_error.lock().unwrap() = None;
                self.remember_close_failure(old_sid, Some(session_id.to_owned()), &error);
                tracing::warn!(
                    session_id = %old_sid,
                    target_session_id = %session_id,
                    %error,
                    "previous session close incomplete; kept pending for retry"
                );
                return Err(error);
            }
            self.clear_pending_close();
        }

        // initial 是窗口替换的候选：在接纳并发布前不撤销旧窗口状态/交付身份。
        let limits = self.history_v2_limits();
        if history_v2 && !retain_previous {
            // F2b：load 响应可能晚于服务端 flush 的 live 通知到达；先登记暂存。
            crate::kit::history_v2::live::begin_pending(session_id);
        }
        // V2 load 与窗口/正文共用同一出站 FIFO：requestKey 分配与入队在同一临界区，
        // 队列阻塞时 K+1 不会先于 K 上线。没有调度器时 submit 退化为直发。
        let v2_load = history_v2.then(|| {
            crate::kit::history_v2::outbound::submit_load(|request_key| {
                json!({
                    "sessionId": session_id,
                    "cwd": effective_cwd,
                    "model": model,
                    "_meta": {
                        peri_acp_types::history_v2::HISTORY_META_KEY:
                            crate::kit::history_v2::initial::initial_request_value(&limits, request_key),
                    },
                })
            })
        });
        let (request_key, loaded) = match v2_load {
            None => (
                None,
                self.transport
                    .send_request(
                        "session/load",
                        json!({ "sessionId": session_id, "cwd": effective_cwd, "model": model }),
                    )
                    .await,
            ),
            Some(Ok((request_key, crate::kit::history_v2::outbound::Submit::Queued(receiver)))) => {
                (
                    Some(request_key),
                    receiver.await.unwrap_or_else(|_| {
                        Err(AcpError::new(-32603, "history load scheduler closed"))
                    }),
                )
            }
            Some(Ok((request_key, crate::kit::history_v2::outbound::Submit::Direct(params)))) => (
                Some(request_key),
                self.transport.send_request("session/load", params).await,
            ),
            Some(Err(crate::kit::history_v2::outbound::SubmitError::Closed)) => (
                None,
                Err(AcpError::new(-32603, "history load scheduler closed")),
            ),
        };
        let loaded = match loaded {
            Ok(loaded) => loaded,
            Err(error) => {
                *self.restore_error.lock().unwrap() = Some(error.to_string());
                self.fail_initial_transition(start.generation, session_id, retain_previous);
                transition.disarm();
                if history_v2 {
                    // 失败前已进入暂存的旧流补发必须先并入旧窗口，不能直接清空。
                    settle_failed_initial_projection(&mut projection, &previous_v2, session_id);
                } else if self.projection_mode == ClientProjectionMode::Interactive {
                    crate::kit::session_boundary::project_session_boundary(None);
                }
                return Err(error);
            }
        };
        if let Some(request_key) = request_key {
            // V2 连接的首屏只来自 initial：不调用 V1 游标解码、不回放、不再发 window open。
            let admitted = match crate::kit::history_v2::initial::admit_load_response(
                &loaded,
                session_id,
                request_key,
                &limits,
            ) {
                Ok(state) => state,
                Err(error) => {
                    let acp = AcpError::new(-32603, error.to_string()).with_data(json!({
                        "kind": "history_initial_invalid",
                        "reason": error.reason.to_string(),
                    }));
                    *self.restore_error.lock().unwrap() = Some(acp.to_string());
                    self.fail_initial_transition(start.generation, session_id, retain_previous);
                    transition.disarm();
                    // 服务端已提交候选流；暂存里的新流事件会因 stream 不匹配被丢弃，
                    // 但旧窗口若仍有效，仍允许同流暂存并回并恢复视图。
                    settle_failed_initial_projection(&mut projection, &previous_v2, session_id);
                    // 旧窗口仍 Ready 时保留已发布状态与 live/delivery 身份；候选失败
                    // 不得覆盖它。没有旧窗口时才写入失败事实。
                    if !previous_v2.is_ready() {
                        // 失败状态必须在边界清理之后写入，否则会被会话边界重置覆盖。
                        crate::kit::history_v2::publish_failure(session_id, error.into_failure());
                    }
                    // 服务端已装配会话但客户端不发布；新建/跨会话目标尽力回收，
                    // 避免孤儿会话。同会话重载不能关闭仍在使用的旧会话：那会连同
                    // 刚恢复的窗口与 readTicket 一起失效。
                    if !retain_previous {
                        let _ = self
                            .transport
                            .send_request("session/close", json!({"sessionId": session_id}))
                            .await;
                    }
                    return Err(acp);
                }
            };
            let execution_ready = admitted.execution_ready;
            self.lifecycle
                .commit_stable(start.generation, session_id.to_string());
            // F2b：initial 一旦通过原子校验就立即发布首屏（含 initial 前暂存的 live
            // 重放），不等待 user-input 运行时初始化；后者失败只关闭执行事实，不回滚
            // 已经拿到的历史页。
            let restore = admitted.clone();
            let next_identity =
                crate::kit::history_v2::consumer::HistoryV2WindowIdentity::from_state(&restore);
            // 候选项已经通过全部校验：此刻才提交替换，清掉旧导航代次/在途 open，
            // 并发布新首屏。
            crate::kit::history_v2::consumer::reset_consumer_state();
            crate::kit::history_v2::publish_initial(admitted);
            // 新首屏发布后，旧窗口身份才进入关闭队列；Close 幂等。
            if let Some(previous) =
                crate::kit::history_v2::consumer::HistoryV2WindowIdentity::from_state(&previous_v2)
                && Some(&previous) != next_identity.as_ref()
            {
                let _ = crate::kit::history_v2::consumer::request_close_window(previous);
            }
            if let Err(error) = self
                .finish_session_initialization(start.generation, session_id)
                .await
            {
                crate::kit::history_v2::live::clear_pending(session_id);
                let mut restored = restore;
                restored.execution_ready = false;
                if let Some(window) = restored.window.as_mut() {
                    window.paging.live.detached = true;
                    window.paging.status =
                        crate::kit::history_v2::paging::HistoryV2PagingStatus::ReopenRequired {
                            reason: "session initialization failed".into(),
                        };
                }
                crate::kit::history_v2::publish_initial(restored);
                return Err(error);
            }
            // `executionReady=false` 只关闭可执行态；历史仍由 V2 状态模型持有。
            *self.restore_error.lock().unwrap() = if execution_ready {
                None
            } else {
                Some(
                    "session history is readable, but execution is not ready; retry after the session settles"
                        .into(),
                )
            };
        } else {
            // 本次浏览身份（REQ §5.2 `nextCursor` / `snapshotToken`）：只有协商过分页能力的
            // 连接才有这两个字段，未协商时两者都是 None，触发侧因此永不请求历史页。
            // 响应解码失败不改变 load 的成败——浏览能力降级，会话本身已经装配成功。
            let (snapshot_token, next_cursor) =
                crate::kit::history_paging::decode_session_cursor(&loaded);
            // 恢复结果是成功响应的一部分：解码失败只降级提示，不影响会话可用性。
            let recovery = crate::acp_client::client::recovery_view::decode(&loaded);
            crate::kit::history_paging::note_initial_cursor(
                session_id,
                snapshot_token,
                next_cursor,
            );
            self.lifecycle
                .commit_stable(start.generation, session_id.to_string());
            if self.projection_mode == ClientProjectionMode::Interactive {
                // 进入会话先展示已保存占用；缺失字段由解码侧落成未知，不沿用上一会话。
                crate::kit::context_usage::establish_session_usage(
                    crate::kit::context_usage::from_load_response(&loaded),
                );
            }
            self.finish_session_initialization(start.generation, session_id)
                .await?;
            if let Some(view) = recovery.as_ref() {
                crate::acp_client::client::recovery_view::present(session_id, view);
            }
            self.flush_buffered();
            // S5.4：协商过窗口能力且开关开启时，额外建立 V1 窗口（默认关闭时无行为）。
            //
            // 必须在初始化成功之后：服务端对声明 `peri.historyWindowV1` 的连接不再回放历史，
            // 首屏由这次 open 提供；初始化失败会关闭会话，此时 open 没有可交付的首屏。
            crate::kit::history_page_consumer::enqueue_window_open(
                session_id,
                crate::kit::history_paging::WindowDirection::Before,
            );
            *self.restore_error.lock().unwrap() = None;
        }
        self.project_execution_cwd(Some(effective_cwd));
        {
            let mut target = self.startup_restore_target.lock().unwrap();
            if target.as_deref() == Some(session_id) {
                *target = None;
            }
        }
        projection.committed = true;
        transition.disarm();
        if self.projection_mode == ClientProjectionMode::Interactive {
            crate::kit::acp_events::request_bg_task_snapshot();
        }
        Ok(session_id.to_string())
    }

    async fn finish_session_initialization(
        &self,
        generation: u64,
        session_id: &str,
    ) -> Result<(), AcpError> {
        if let Err(error) = self.initialize_user_inputs_under_gate(session_id).await {
            let claims = self.lifecycle.fail_transition(generation);
            self.settle_claims_owned(claims).await;
            self.project_execution_cwd(None);
            *self.restore_error.lock().unwrap() = Some(error.to_string());
            if self.projection_mode == ClientProjectionMode::Interactive {
                crate::kit::session_boundary::project_session_boundary(None);
            }
            if let Err(close_error) = self
                .transport
                .send_request("session/close", json!({"sessionId": session_id}))
                .await
            {
                tracing::warn!(%close_error, "session initialization cleanup failed");
            }
            return Err(error);
        }
        Ok(())
    }

    async fn initialize_user_inputs_under_gate(&self, session_id: &str) -> Result<(), AcpError> {
        if self.supports_user_input_queue() {
            let snapshot = self.user_input_snapshot_under_gate(session_id).await?;
            if self.projection_mode == ClientProjectionMode::Interactive {
                crate::kit::steer_state::establish_session_snapshot(snapshot);
            }
        }
        Ok(())
    }
}
