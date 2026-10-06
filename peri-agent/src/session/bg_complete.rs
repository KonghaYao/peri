//! Frozen, durable background-completion publication routes.
//!
//! Synchronous owners retain a terminal result while publication is Pending.
//! Only a confirmed reliable Inbox receipt permits acknowledgment; route
//! identity is captured once and cannot follow a reopened session lifecycle.

use std::sync::Arc;

use peri_acp_types::session::SessionAccessPort;
use peri_acp_types::tasks::BgTaskKind;

use crate::agent::events::BackgroundTaskResult;
use crate::session::async_router::background_result_reminder;
use crate::session::factory::OnBgCompleteFn;

/// 由 `(SessionAccessPort, session_id)` 构造 **session 级** `on_bg_complete` 回调。
///
/// 构造时冻结可靠投递路由；缺失路由不得转到未来生命周期。
/// Pending 返回 Err，owner 必须保留原结果并重试；持久回执确认后才返回 Ok。
pub fn session_bg_complete_callback(
    session_access: Arc<dyn SessionAccessPort>,
    session_id: String,
) -> OnBgCompleteFn {
    let delivery = session_access.task_terminal_delivery(&session_id);
    publication_callback(Arc::new(move || {
        delivery.clone().ok_or_else(|| {
            format!("frozen durable terminal delivery unavailable for session {session_id}")
        })
    }))
}

pub fn durable_bg_complete_callback(
    delivery: Arc<dyn peri_acp_types::tasks::TaskTerminalDelivery>,
) -> OnBgCompleteFn {
    publication_callback(Arc::new(move || Ok(Arc::clone(&delivery))))
}

type DeliveryResolver = Arc<
    dyn Fn() -> Result<Arc<dyn peri_acp_types::tasks::TaskTerminalDelivery>, String> + Send + Sync,
>;

enum PublicationStatus {
    Pending,
    Accepted,
    Failed(String),
}

struct Publication {
    fingerprint: String,
    status: PublicationStatus,
}

fn publication_callback(resolve: DeliveryResolver) -> OnBgCompleteFn {
    let publications = Arc::new(parking_lot::Mutex::new(std::collections::HashMap::<
        String,
        Publication,
    >::new()));
    Arc::new(move |result: &BackgroundTaskResult, kind: BgTaskKind| {
        let delivery_id =
            crate::agent::async_tasks::delivery::terminal_delivery_id(&result.task_id, "terminal");
        let identity = delivery_id.as_uuid().to_string();
        let reminder = background_result_reminder(result, kind);
        let fingerprint =
            serde_json::to_string(reminder.as_reminder()).map_err(|error| error.to_string())?;
        let mut state = publications.lock();
        if let Some(prior) = state.get(&identity) {
            if prior.fingerprint != fingerprint {
                return Err("conflicting terminal publication identity".into());
            }
            match &prior.status {
                PublicationStatus::Accepted => return Ok(()),
                PublicationStatus::Pending => {
                    return Err(
                        "terminal publication Pending: durable receipt not confirmed".into(),
                    )
                }
                PublicationStatus::Failed(error) => {
                    tracing::info!(
                        ?delivery_id,
                        task_id = %result.task_id,
                        %error,
                        "retrying original terminal publication"
                    )
                }
            }
        }
        let delivery = resolve()?;
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|_| "terminal publication runtime unavailable".to_string())?;
        state.insert(
            identity.clone(),
            Publication {
                fingerprint,
                status: PublicationStatus::Pending,
            },
        );
        drop(state);
        let publications = Arc::clone(&publications);
        let source = match kind {
            BgTaskKind::Agent => peri_acp_types::session::MessageSource::SubAgentComplete,
            BgTaskKind::Shell => peri_acp_types::session::MessageSource::ShellComplete,
            BgTaskKind::Workflow => peri_acp_types::session::MessageSource::WorkflowComplete,
            BgTaskKind::Mcp => peri_acp_types::session::MessageSource::DynamicMcpNotification,
        };
        let task_id = result.task_id.clone();
        runtime.spawn(async move {
            let outcome = delivery.deliver(delivery_id, &reminder, source).await;
            let mut state = publications.lock();
            if let Some(publication) = state.get_mut(&identity) {
                publication.status = match &outcome {
                    Ok(()) => PublicationStatus::Accepted,
                    Err(error) => PublicationStatus::Failed(error.clone()),
                };
            }
            drop(state);
            if let Err(error) = &outcome {
                tracing::warn!(
                    ?delivery_id,
                    task_id = %task_id,
                    %error,
                    "terminal publication failed"
                );
            }
        });
        Err(
            "terminal publication Pending: owner must retain and retry until durable receipt"
                .into(),
        )
    })
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "bg_complete_test.rs"]
mod tests;
