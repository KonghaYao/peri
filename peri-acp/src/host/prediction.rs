//! Prediction task admission, model invocation, and session metadata projection.

use std::sync::Arc;

use peri_acp_types::event_data::PredictionAction;

use super::{notify, prediction_projection, task_scope, AcpServerConfig, SharedSessions};

pub(super) fn spawn_prediction(
    transport: &Arc<dyn crate::transport::AcpTransport>,
    prompt_session_id: &str,
    sessions: &SharedSessions,
    cfg: &AcpServerConfig,
) {
    let pred_transport = Arc::clone(transport);
    let pred_session_id = prompt_session_id.to_string();
    let pred_provider = cfg.provider.clone();
    let pred_sessions = sessions.clone();
    let pred_resources = cfg.session_resources.clone();
    let pred_caps_registry = cfg.session_manager.caps_registry();
    // 宿主 scope 的关停信号：关停方（如 `session/close` 经
    // `SessionEnvironment::shutdown`）会持 sessions 锁等待本会话任务收摊，本任务
    // 若在关停开始后继续排队等同一把锁即互等，只能耗到协作宽限（5s）被强杀。
    // 预测动作与通知只对活跃会话有意义，关停开始即放弃本轮。
    let pred_shutdown = cfg.host_task_spawner.shutdown_token();

    let _ = cfg.host_task_spawner.spawn(
        task_scope::HostTaskOwnerKind::Session,
        task_scope::HostTaskKind::Prediction,
        async move {
            tracing::debug!("Prediction task started");
            // 从 session 获取最新历史与当前标题
            let (history, cwd, current_title) = {
                let Some(sessions) = lock_or_shutdown(&pred_sessions, &pred_shutdown).await else {
                    tracing::debug!("Prediction: host scope shutting down, dropping prediction");
                    return;
                };
                match sessions.get(&pred_session_id) {
                    Some(s) => (s.history.clone(), s.cwd.clone(), s.title.clone()),
                    None => {
                        tracing::debug!("Prediction: session not found");
                        return;
                    }
                }
            };

            // 最近 10 条非 System 消息是软窗口；工具调用 batch 会完整扩展，
            // 历史中本就不完整的 batch 则整组丢弃。
            let recent = prediction_projection::project_prediction_history(&history);

            if recent.is_empty() {
                tracing::debug!("Prediction: no recent messages");
                return;
            }
            tracing::debug!(count = recent.len(), "Prediction: got messages");

            // 直接复用已构建的 LlmProvider（绕过 from_config）
            let llm_provider = pred_provider.read().clone();
            tracing::debug!("Prediction: LLM provider ready");

            // Facade：agent 构建与执行统一由 peri-acp executor 承担，
            // TUI 层不再直接构建 Agent（遵守 CLAUDE.md [TRAP]）。
            // L5：LLM 构造（AgentModelBridge）在协议面完成，执行体只收 ReactLLM。
            let llm: Box<dyn peri_agent::agent::react::ReactLLM + Send + Sync> =
                Box::new(peri_agent::agent::model_bridge::AgentModelBridge::new(
                    Arc::from(llm_provider.into_model()),
                ));
            let result = crate::session::executor::execute_prediction(
                llm,
                recent,
                &cwd,
                current_title.as_deref(),
            )
            .await;

            match result {
                Ok(actions) => {
                    if actions.is_empty() {
                        tracing::debug!("Prediction: empty actions");
                        return;
                    }
                    // 元数据动作写入 session 状态；标题变更待持久化并推送
                    let mut applied_title: Option<String> = None;
                    {
                        let Some(mut sessions) =
                            lock_or_shutdown(&pred_sessions, &pred_shutdown).await
                        else {
                            tracing::debug!(
                                "Prediction: host scope shutting down, dropping metadata actions"
                            );
                            return;
                        };
                        if let Some(state) = sessions.get_mut(&pred_session_id) {
                            for action in &actions {
                                match action {
                                    PredictionAction::SetTitle { title } => {
                                        let title = title.trim();
                                        if !title.is_empty() {
                                            state.title = Some(title.to_string());
                                            applied_title = Some(title.to_string());
                                        }
                                    }
                                    PredictionAction::AddTag { tag }
                                        if !state.tags.contains(tag) =>
                                    {
                                        state.tags.push(tag.clone());
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                    // 标题变更：经门面定向持久化 metadata（不整份覆盖），并推送
                    // session/update 供标题栏与外部客户端刷新（与 session/rename 同行为）
                    if let Some(title) = applied_title {
                        if let Err(e) = pred_resources
                            .update_session_meta(
                                &pred_session_id,
                                &peri_acp_types::session_resources::SessionMetaPatch {
                                    title: Some(Some(title.clone())),
                                    ..Default::default()
                                },
                            )
                            .await
                        {
                            tracing::warn!(
                                session_id = %pred_session_id,
                                error = %e,
                                "Prediction: failed to persist title"
                            );
                        }
                        notify::send_session_info_update_with_title(
                            pred_transport.as_ref(),
                            &pred_session_id,
                            Some(&title),
                        )
                        .await;
                    }
                    let caps = pred_caps_registry
                        .get(&pred_session_id)
                        .map(|r| r.clone())
                        .unwrap_or_default();
                    if caps.prediction {
                        // text 字段取首个 Placeholder（兼容旧消费方）
                        let text = actions
                            .iter()
                            .find_map(|a| match a {
                                PredictionAction::Placeholder { text } => Some(text.clone()),
                                _ => None,
                            })
                            .unwrap_or_default();
                        let actions_json: Vec<serde_json::Value> = actions
                            .iter()
                            .filter_map(|a| serde_json::to_value(a).ok())
                            .collect();
                        tracing::debug!(
                            count = actions.len(),
                            "Prediction ready, sending notification"
                        );
                        let _ = pred_transport
                            .send_notification(
                                "peri/prediction_ready",
                                serde_json::json!({
                                    "sessionId": pred_session_id,
                                    "text": text,
                                    "actions": actions_json,
                                }),
                            )
                            .await;
                    } else {
                        tracing::debug!(
                            "Prediction ready but cap not declared, suppressing notification"
                        );
                    }
                }
                Err(crate::session::executor::PredictionError::Failed(e)) => {
                    tracing::debug!(error = %e, "Prediction task failed");
                }
                Err(crate::session::executor::PredictionError::Timeout) => {
                    tracing::debug!("Prediction task timed out (30s)");
                }
            }
        },
    );
}

/// 获取 sessions 锁，同时观察宿主 scope 的关停信号：关停开始即返回 `None`。
///
/// 关停由 `session/close` 等持有 sessions 锁的调用方发起
/// （`SessionEnvironment::shutdown` → `HostTaskOwner::shutdown`），它们在同一段代码里
/// 等待本会话任务收摊。任务若在关停开始后继续排队等同一把锁，双方即互等，只能被
/// 协作宽限后的强杀解开。预测结果只服务于活跃会话，关停后丢弃是正确语义
/// （`biased` 让关停判定优先于恰好空闲的锁，行为不随调度抖动）。
async fn lock_or_shutdown<'a, T>(
    sessions: &'a tokio::sync::Mutex<T>,
    shutdown: &tokio_util::sync::CancellationToken,
) -> Option<tokio::sync::MutexGuard<'a, T>> {
    tokio::select! {
        biased;
        () = shutdown.cancelled() => None,
        guard = sessions.lock() => Some(guard),
    }
}

#[cfg(test)]
#[path = "prediction_test.rs"]
mod tests;
