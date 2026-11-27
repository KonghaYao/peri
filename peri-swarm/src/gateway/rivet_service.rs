//! [`SwarmService`] adapter that routes every call to the session's Rivet actor.
//!
//! Routing key: `ActorKey::new(logicalId).segments()` (`design §3.3`). The
//! actor key is stable per `logicalId`, so `open_stream` — which the HTTP layer
//! calls **before** `create_session` — addresses the same actor the create will
//! use. `rootSessionId` comes from the store (or is minted once for a brand-new
//! session) and is passed to the actor as the reserved ACP id.
//!
//! The adapter keeps no per-session state. Subscriptions are one forwarded task
//! per SSE client: dropping the client drops its broadcast receiver, the task
//! stops, and the actor's HTTP callback ends. Turns are unaffected.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use reqwest::header::HeaderMap;
use reqwest::Method;
use rivetkit::client::{Client, GetOrCreateOptions};
use rivetkit::TypedActorHandle;
use tokio::sync::broadcast;

use crate::actor::swarm::{
    ActionEnvelope, Create, Delete, Get, GetSubagent, GetTurn, ListItems, ListSubagents, ListTurns,
    RespondRequiredAction, Submit, Subscribe, SubscribeOutcome, SwarmActor, Update, ACTOR_NAME,
};
use crate::domain::{
    new_root_session_id, ActorKey, Cursor, EventCursor, InputOutcome, InputSubmission, ItemPage,
    ItemsWindow, LogicalId, OfficialSubagent, OfficialTurn, SessionPage, SessionView,
    SessionsWindow, StreamEvent, SubagentPage, SubagentsWindow, SwarmError, TurnPage, TurnsWindow,
};
use crate::event_stream::FrameParser;
use crate::service::{
    ApprovalReceipt, CreateSessionCommand, CreateSessionResult, EventSubscription,
    SessionUpdateCommand, SwarmService,
};
use crate::store::{store_failure, SwarmStore};

/// Adapter configuration. The channel capacity bounds how far a slow SSE client
/// may fall behind before it receives a `gap` event.
#[derive(Clone, Debug)]
pub struct RivetServiceConfig {
    pub channel_capacity: usize,
}

impl Default for RivetServiceConfig {
    fn default() -> Self {
        Self {
            channel_capacity: 256,
        }
    }
}

pub struct RivetSwarmService {
    client: Client,
    store: Arc<dyn SwarmStore>,
    config: RivetServiceConfig,
}

impl std::fmt::Debug for RivetSwarmService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RivetSwarmService")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl RivetSwarmService {
    /// `client` must be built with the same pool as the registry that serves
    /// actors; `store` is the same deployment store the actors write.
    #[must_use]
    pub fn new(client: Client, store: Arc<dyn SwarmStore>, config: RivetServiceConfig) -> Self {
        Self {
            client,
            store,
            config,
        }
    }

    fn handle(&self, logical_id: &LogicalId) -> Result<TypedActorHandle<SwarmActor>, SwarmError> {
        let key: Vec<String> = ActorKey::new(logical_id.clone())
            .segments()
            .iter()
            .map(|segment| (*segment).to_owned())
            .collect();
        let handle = self
            .client
            .get_or_create(ACTOR_NAME, key, GetOrCreateOptions::default())
            .map_err(|error| {
                tracing::warn!(
                    logical_id = logical_id.as_str(),
                    error = %error,
                    "swarm actor handle resolution failed"
                );
                SwarmError::upstream_unavailable("swarm_actor_handle")
            })?;
        Ok(TypedActorHandle::new(handle))
    }

    async fn call<M>(
        &self,
        handle: &TypedActorHandle<SwarmActor>,
        action: M,
    ) -> Result<M::Output, SwarmError>
    where
        SwarmActor: rivetkit::Handler<M>,
        M: rivetkit::Action,
    {
        handle.send(action).await.map_err(|error| {
            tracing::warn!(
                action = M::NAME,
                error = %error,
                "swarm actor action failed"
            );
            SwarmError::upstream_unavailable("swarm_actor")
        })
    }

    async fn require_row(
        &self,
        logical_id: &LogicalId,
    ) -> Result<crate::store::SwarmSessionRow, SwarmError> {
        self.store
            .swarm_get_session(logical_id.as_str())
            .await
            .map_err(|error| store_failure("get_session", error))?
            .ok_or_else(|| SwarmError::unknown_agent(logical_id.as_str()))
    }

    /// Open the actor event stream and forward decoded events to a broadcast
    /// channel. The actor replays its own backlog, so the returned subscription
    /// has no local backlog.
    ///
    /// [`EventCursor::Tail`] sends **no** `afterSeq`: the actor then starts at
    /// its tail and forwards live events only.
    async fn open(
        &self,
        logical_id: &LogicalId,
        start: EventCursor,
    ) -> Result<EventSubscription, SwarmError> {
        let handle = self.handle(logical_id)?;
        let path = match start.cursor() {
            Some(cursor) => format!("/events?afterSeq={}", cursor.value()),
            None => "/events".to_owned(),
        };
        let response = handle
            .inner()
            .fetch(&path, Method::GET, HeaderMap::new(), None)
            .await
            .map_err(|error| {
                tracing::warn!(
                    logical_id = logical_id.as_str(),
                    cursor = start.cursor().map_or(0, Cursor::value),
                    error = %error,
                    "swarm actor event stream failed"
                );
                SwarmError::upstream_unavailable("swarm_actor_stream")
            })?;
        if !response.status().is_success() {
            tracing::warn!(
                logical_id = logical_id.as_str(),
                status = response.status().as_u16(),
                "swarm actor refused the event stream"
            );
            return Err(SwarmError::upstream_unavailable("swarm_actor_stream"));
        }
        let (sender, receiver) = broadcast::channel(self.config.channel_capacity.max(1));
        // The actor replays from the cursor, so the subscription tail starts at
        // the stored MAX(seq) (or the client cursor if the store read fails)
        // and advances with every forwarded item.
        let tail = Arc::new(AtomicU64::new(
            match self.store.swarm_last_seq(logical_id.as_str()).await {
                Ok(seq) => seq,
                Err(error) => {
                    tracing::warn!(
                        logical_id = logical_id.as_str(),
                        error = %error,
                        "swarm last seq read failed; heartbeat starts from the cursor"
                    );
                    start.cursor().map_or(0, Cursor::value)
                }
            },
        ));
        tokio::spawn(forward_stream(response, sender, Arc::clone(&tail)));
        Ok(EventSubscription::new(Vec::new(), receiver, tail))
    }
}

/// Parse the actor SSE stream and forward domain events. Ends when the client
/// side (all broadcast receivers) or the actor stream goes away.
async fn forward_stream(
    response: reqwest::Response,
    sender: broadcast::Sender<StreamEvent>,
    tail: Arc<AtomicU64>,
) {
    let mut parser = FrameParser::new();
    let mut chunks = response.bytes_stream();
    loop {
        let chunk = tokio::select! {
            biased;
            () = sender.closed() => return,
            chunk = chunks.next() => chunk,
        };
        let Some(chunk) = chunk else { return };
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(error) => {
                tracing::warn!(error = %error, "swarm actor stream read failed");
                return;
            }
        };
        for frame in parser.push(&chunk) {
            let Some(event) = frame.into_stream_event() else {
                continue;
            };
            if let Some(seq) = event.item_seq() {
                // Persisted by the actor before the frame was sent.
                tail.fetch_max(seq, Ordering::Relaxed);
            }
            if sender.send(event).is_err() {
                // No viewer left: dropping the response closes the actor
                // callback. The turn itself is not cancelled (`design §6.2`).
                return;
            }
        }
    }
}

impl RivetSwarmService {
    async fn respond_required_action(
        &self,
        logical_id: &LogicalId,
        request_id: &str,
        decision: crate::domain::ApprovalDecision,
        input_id: Option<String>,
    ) -> Result<ApprovalReceipt, SwarmError> {
        let handle = self.handle(logical_id)?;
        let envelope: ActionEnvelope<ApprovalReceipt> = self
            .call(
                &handle,
                RespondRequiredAction {
                    logical_id: logical_id.as_str().to_owned(),
                    request_id: request_id.to_owned(),
                    decision,
                    input_id,
                },
            )
            .await?;
        envelope.into_result()
    }
}

#[async_trait]
impl SwarmService for RivetSwarmService {
    async fn create_session(
        &self,
        command: CreateSessionCommand,
    ) -> Result<CreateSessionResult, SwarmError> {
        let logical_id = command.logical_id.clone();
        // `logicalId → rootSessionId` comes from the store: a retried create
        // must address the identity that may already exist, never mint a second
        // one for the same logical id.
        let existing = self.require_row_optional(&logical_id).await?;
        let root_session_id = existing
            .as_ref()
            .map(|row| row.root_session_id.clone())
            .or(command.root_session_id.clone())
            .unwrap_or_else(new_root_session_id);
        let handle = self.handle(&logical_id)?;
        let action = Create {
            logical_id: logical_id.as_str().to_owned(),
            cwd: command.cwd.to_string_lossy().into_owned(),
            model: command.model,
            instructions: command.instructions,
            mcp_tools: command.mcp_tools,
            metadata: command.metadata,
            prompt_cache_options: command.prompt_cache_options,
            initial_input: command.initial_input,
            environment: command.environment,
            environment_id: command.environment_id,
            root_session_id: Some(root_session_id),
            defer_initial_turn: command.defer_initial_turn,
        };
        let envelope: ActionEnvelope<CreateSessionResult> = self.call(&handle, action).await?;
        envelope.into_result()
    }

    async fn get_session(&self, logical_id: &LogicalId) -> Result<SessionView, SwarmError> {
        let handle = self.handle(logical_id)?;
        let envelope: ActionEnvelope<SessionView> = self
            .call(
                &handle,
                Get {
                    logical_id: logical_id.as_str().to_owned(),
                },
            )
            .await?;
        envelope.into_result()
    }

    async fn list_sessions(&self, window: SessionsWindow) -> Result<SessionPage, SwarmError> {
        // `sessions.list` is cross-session: no actor owns the result set, so
        // the gateway reads the same store the actors write.
        crate::service::list_sessions_from_store(self.store.as_ref(), &window).await
    }

    async fn update_session(
        &self,
        logical_id: &LogicalId,
        command: SessionUpdateCommand,
    ) -> Result<SessionView, SwarmError> {
        let handle = self.handle(logical_id)?;
        let action = Update {
            logical_id: logical_id.as_str().to_owned(),
            model: command.model,
            mcp_tools: command.mcp_tools,
            metadata: command.metadata,
            prompt_cache_options: command.prompt_cache_options,
        };
        let envelope: ActionEnvelope<SessionView> = self.call(&handle, action).await?;
        envelope.into_result()
    }

    async fn delete_session(&self, logical_id: &LogicalId) -> Result<(), SwarmError> {
        let handle = self.handle(logical_id)?;
        let action = Delete {
            logical_id: logical_id.as_str().to_owned(),
        };
        let envelope: ActionEnvelope<crate::domain::SessionDeleted> =
            self.call(&handle, action).await?;
        envelope.into_result().map(|_| ())
    }

    async fn list_turns(
        &self,
        logical_id: &LogicalId,
        window: TurnsWindow,
    ) -> Result<TurnPage, SwarmError> {
        let handle = self.handle(logical_id)?;
        let action = ListTurns {
            logical_id: logical_id.as_str().to_owned(),
            order: window.order,
            after: window.after,
            limit: window.limit,
        };
        let envelope: ActionEnvelope<TurnPage> = self.call(&handle, action).await?;
        envelope.into_result()
    }

    async fn get_turn(
        &self,
        logical_id: &LogicalId,
        turn_id: &str,
    ) -> Result<OfficialTurn, SwarmError> {
        let handle = self.handle(logical_id)?;
        let action = GetTurn {
            logical_id: logical_id.as_str().to_owned(),
            turn_id: turn_id.to_owned(),
        };
        let envelope: ActionEnvelope<OfficialTurn> = self.call(&handle, action).await?;
        envelope.into_result()
    }

    async fn list_subagents(
        &self,
        logical_id: &LogicalId,
        window: SubagentsWindow,
    ) -> Result<SubagentPage, SwarmError> {
        let handle = self.handle(logical_id)?;
        let action = ListSubagents {
            logical_id: logical_id.as_str().to_owned(),
            order: window.order,
            after: window.after,
            limit: window.limit,
        };
        let envelope: ActionEnvelope<SubagentPage> = self.call(&handle, action).await?;
        envelope.into_result()
    }

    async fn get_subagent(
        &self,
        logical_id: &LogicalId,
        subagent_id: &str,
    ) -> Result<OfficialSubagent, SwarmError> {
        let handle = self.handle(logical_id)?;
        let action = GetSubagent {
            logical_id: logical_id.as_str().to_owned(),
            subagent_id: subagent_id.to_owned(),
        };
        let envelope: ActionEnvelope<OfficialSubagent> = self.call(&handle, action).await?;
        envelope.into_result()
    }

    async fn accept_input(
        &self,
        logical_id: &LogicalId,
        submission: InputSubmission,
    ) -> Result<crate::service::AcceptedInput, SwarmError> {
        let handle = self.handle(logical_id)?;
        let action = Submit {
            logical_id: logical_id.as_str().to_owned(),
            input_id: submission.input_id,
            kind: submission.kind,
            mode: submission.mode,
            text: submission.text,
            attachments: submission.attachments,
            plugin_selections: submission.plugin_selections,
            original_draft: submission.original_draft,
        };
        let envelope: ActionEnvelope<InputOutcome> = self.call(&handle, action).await?;
        // The actor runs the accepted turn itself (it owns the ACP host), so the
        // gateway never receives pending work.
        envelope
            .into_result()
            .map(crate::service::AcceptedInput::settled)
    }

    async fn run_pending_turn(
        &self,
        _pending: crate::service::PendingTurn,
    ) -> Result<(), SwarmError> {
        // Pending turns are executed inside the actor; a gateway-side call means
        // the acceptance contract was bypassed.
        Err(SwarmError::upstream_unavailable("pending_turn_not_routed"))
    }

    async fn list_items(
        &self,
        logical_id: &LogicalId,
        window: ItemsWindow,
    ) -> Result<ItemPage, SwarmError> {
        let handle = self.handle(logical_id)?;
        let action = ListItems {
            logical_id: logical_id.as_str().to_owned(),
            order: window.order,
            after: window.after,
            after_id: window.after_id,
            limit: window.limit,
        };
        let envelope: ActionEnvelope<ItemPage> = self.call(&handle, action).await?;
        envelope.into_result()
    }

    async fn subscribe(
        &self,
        logical_id: &LogicalId,
        start: EventCursor,
    ) -> Result<EventSubscription, SwarmError> {
        // Unknown sessions are 404 before any stream is opened; the row also
        // carries the `rootSessionId` the actor must be hosting.
        let row = self.require_row(logical_id).await?;
        tracing::debug!(
            logical_id = logical_id.as_str(),
            root_session_id = %row.root_session_id,
            after_seq = start.cursor().map_or(0, Cursor::value),
            replay = start.cursor().is_some(),
            "swarm gateway subscribing through the actor"
        );
        self.open(logical_id, start).await
    }

    async fn open_stream(&self, logical_id: &LogicalId) -> EventSubscription {
        // Tail: the create response must not replay history from before this
        // stream (`design §4.3`).
        match self.open(logical_id, EventCursor::Tail).await {
            Ok(subscription) => subscription,
            Err(error) => {
                // The HTTP layer reports `create_session`'s failure as JSON;
                // this stream never produces events and closes immediately.
                tracing::warn!(
                    logical_id = logical_id.as_str(),
                    code = error.code().as_str(),
                    "swarm gateway could not open the actor stream before create"
                );
                let (sender, receiver) = broadcast::channel(1);
                drop(sender);
                EventSubscription::new(Vec::new(), receiver, Arc::new(AtomicU64::new(0)))
            }
        }
    }

    async fn respond_required_action(
        &self,
        logical_id: &LogicalId,
        request_id: &str,
        decision: crate::domain::ApprovalDecision,
        input_id: Option<String>,
    ) -> Result<ApprovalReceipt, SwarmError> {
        RivetSwarmService::respond_required_action(self, logical_id, request_id, decision, input_id)
            .await
    }
}

impl RivetSwarmService {
    async fn require_row_optional(
        &self,
        logical_id: &LogicalId,
    ) -> Result<Option<crate::store::SwarmSessionRow>, SwarmError> {
        self.store
            .swarm_get_session(logical_id.as_str())
            .await
            .map_err(|error| store_failure("get_session", error))
    }

    /// Incremental read used when a stream reports a gap: returns the next page
    /// of persisted items after `after`.
    pub async fn read_items(
        &self,
        logical_id: &LogicalId,
        after: u64,
        limit: u32,
    ) -> Result<SubscribeOutcome, SwarmError> {
        let handle = self.handle(logical_id)?;
        let envelope: ActionEnvelope<SubscribeOutcome> = self
            .call(
                &handle,
                Subscribe {
                    logical_id: logical_id.as_str().to_owned(),
                    after_seq: after,
                    limit,
                },
            )
            .await?;
        envelope.into_result()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn idle_sse_stops_upstream_when_last_viewer_leaves() {
        let bytes = futures::stream::pending::<Result<Vec<u8>, std::io::Error>>();
        let body = reqwest::Body::wrap_stream(bytes);
        let response = reqwest::Response::from(axum::http::Response::new(body));
        let (events, viewer) = broadcast::channel(1);
        let last_viewer = events.subscribe();
        let task = forward_stream(response, events, Arc::new(AtomicU64::new(0)));
        tokio::pin!(task);
        assert!(futures::poll!(&mut task).is_pending());
        drop(viewer);
        assert!(futures::poll!(&mut task).is_pending());
        drop(last_viewer);
        assert!(futures::poll!(&mut task).is_ready());
    }
}
