//! Turn telemetry hooks and the event forwarder share one tracer.
use crate::session::executor;
use peri_agent::session::exec::executor_helpers::ForwarderLauncherFn;
use peri_controller::langfuse::{bridge::LangfuseBridge, tracer::LangfuseTracer, LangfuseSession};
use std::sync::Arc;

pub(super) fn build_langfuse_hooks(
    langfuse_session: Option<&Arc<LangfuseSession>>,
    session_id: &str,
) -> Option<executor::LangfuseHooks> {
    langfuse_session.map(|s| {
        let session_clone = Arc::clone(s);
        let config = session_clone.config.clone();
        let session: std::sync::Arc<dyn peri_controller::langfuse::LangfuseSessionLike> =
            session_clone;
        let tracer = Arc::new(parking_lot::Mutex::new(LangfuseTracer::new(
            session,
            session_id.to_owned(),
            config,
        )));
        executor::LangfuseHooks {
            on_turn_start: {
                let tracer = Arc::clone(&tracer);
                Arc::new(move |input: &str| {
                    tracer.lock().on_turn_start(input);
                }) as Arc<dyn Fn(&str) + Send + Sync>
            },
            on_turn_end: {
                let tracer = Arc::clone(&tracer);
                Arc::new(
                    move |outcome: peri_acp_types::session::TurnTelemetryOutcome| {
                        tracer.lock().on_turn_end(outcome).into()
                    },
                )
                    as Arc<
                        dyn Fn(
                                peri_acp_types::session::TurnTelemetryOutcome,
                            ) -> Option<tokio::task::JoinHandle<()>>
                            + Send
                            + Sync,
                    >
            },
            bridge_factory: {
                let tracer = Arc::clone(&tracer);
                Arc::new(move |name: String, agent_id: Option<String>| {
                    Some(
                        Arc::new(LangfuseBridge::new(Arc::clone(&tracer), name, agent_id))
                            as Arc<dyn peri_agent::agent::LangfuseBridgeLike>,
                    )
                })
                    as Arc<
                        dyn Fn(
                                String,
                                Option<String>,
                            )
                                -> Option<Arc<dyn peri_agent::agent::LangfuseBridgeLike>>
                            + Send
                            + Sync,
                    >
            },
        }
    })
}

pub(super) fn build_forwarder_launcher(
    provider_name: &str,
    langfuse_hooks: Option<&executor::LangfuseHooks>,
) -> ForwarderLauncherFn {
    {
        let provider_display = provider_name.to_owned();
        let bridge_factory = langfuse_hooks.map(|h| Arc::clone(&h.bridge_factory));
        Arc::new(move |handles, agent_id, on_event| {
            let bridge: Option<LangfuseBridge> = bridge_factory
                .as_ref()
                .and_then(|bf| bf(provider_display.clone(), Some(agent_id.clone())))
                .and_then(|b| {
                    // LangfuseBridgeLike: Any 上界（L5）——trait upcasting 还原具体类型
                    let any: Arc<dyn std::any::Any + Send + Sync> = b;
                    any.downcast::<LangfuseBridge>().ok().map(|b| (*b).clone())
                });
            crate::event::spawn_eventbus_forwarder(handles, on_event, bridge)
        })
    }
}
