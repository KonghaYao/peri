//! Rivet actor hosting one Swarm session (S5).
//!
//! Actor identity (`design §3.3`): name [`ACTOR_NAME`], key `["agent", logicalId]`.
//! The key is the only identity this actor trusts. Actor input is a deployment
//! hint that may be absent when the engine restarts the actor, so every
//! decision falls back to the key plus `swarm_sessions`.
//!
//! This module owns no ACP state yet; S5 stage 2 attaches
//! [`crate::actor::service::AcpSwarmService`] and the gateway adapter. Keeping the
//! registration here means the gateway and the tests share one actor name,
//! one sleep policy, and one fetch surface.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use rivetkit::prelude::{anyhow, Result};
use rivetkit::{
    Actor, ActorConfig, ActorHttpResponse, ActorKeySegment, Ctx, Handler, Registry, Request,
    Response,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::domain::{ActorKey, LogicalId};

/// Registered actor name. It is part of the routing contract with the gateway.
pub const ACTOR_NAME: &str = ActorKey::ACTOR_NAME;

/// Explicit idle timeout. The engine default is not a contract (`design §6.3`).
pub const SLEEP_TIMEOUT: Duration = Duration::from_secs(300);

/// Optional deployment hint carried by `get_or_create`.
///
/// Every field is optional: the engine may restart an actor without input, and
/// `create` must still work from the key plus the store (`design §3.3`).
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SwarmActorInput {
    pub logical_id: Option<String>,
    pub cwd: Option<String>,
    pub model: Option<String>,
    pub instructions: Option<String>,
}

/// Actor factory configuration: stateless actor (the runtime lives in memory),
/// explicit 300 s sleep timeout.
#[must_use]
pub fn actor_config() -> ActorConfig {
    ActorConfig {
        has_state: false,
        sleep_timeout: SLEEP_TIMEOUT,
        ..ActorConfig::default()
    }
}

/// Registry pre-populated with the Swarm actor.
#[must_use]
pub fn registry() -> Registry {
    let mut registry = Registry::new();
    registry.register_actor_with::<SwarmActor>(ACTOR_NAME, actor_config());
    registry
}

/// One Swarm session hosted by Rivet.
pub struct SwarmActor {
    logical_id: LogicalId,
}

impl SwarmActor {
    #[must_use]
    pub fn logical_id(&self) -> &LogicalId {
        &self.logical_id
    }

    /// Identity from the actor key. `ActorKeySegment::Number` cannot be a
    /// `logicalId`, so it is rejected instead of coerced to text.
    fn logical_id_from_key(ctx: &Ctx<Self>) -> Result<LogicalId> {
        let key = ctx.key();
        let [ActorKeySegment::String(namespace), ActorKeySegment::String(logical_id)] =
            key.as_slice()
        else {
            return Err(anyhow!(
                "swarm actor key must be [\"agent\", <logicalId>], got {key:?}"
            ));
        };
        if namespace != "agent" {
            return Err(anyhow!(
                "swarm actor namespace segment must be \"agent\", got {namespace:?}"
            ));
        }
        LogicalId::parse(logical_id).map_err(|error| anyhow!(error.to_string()))
    }
}

#[async_trait]
impl Actor for SwarmActor {
    type Input = SwarmActorInput;
    type Actions = (Info,);
    type Events = ();
    type ConnParams = ();
    type Action = Value;

    /// The SSE endpoint must stay open while actions keep running, so HTTP
    /// callbacks are dispatched off the actor event loop (`rivetkit` default is
    /// serialized) and the live pool is sized for concurrent viewers.
    const CONCURRENT_HTTP_CALLBACKS: bool = true;
    const MAX_CONCURRENT_HTTP_CALLBACKS: usize = 64;
    const MAX_CONCURRENT_LIVE_HTTP_CALLBACK_STARTS: usize = 32;

    async fn create(ctx: &Ctx<Self>) -> Result<Self> {
        let logical_id = Self::logical_id_from_key(ctx)?;
        // The deployment hint may be missing or stale on an engine restart; it
        // is never used as identity.
        let _input: SwarmActorInput = ctx.input().unwrap_or_default();
        Ok(Self { logical_id })
    }

    async fn on_fetch_response(
        self: Arc<Self>,
        _ctx: Ctx<Self>,
        request: Request,
    ) -> Result<ActorHttpResponse> {
        let path = request.uri().path().to_owned();
        if request.method().as_str() == "GET" && path == "/health" {
            let body = serde_json::to_vec(&InfoOutput {
                logical_id: self.logical_id.as_str().to_owned(),
                actor_key: actor_key_segments(&self.logical_id),
            })?;
            let response = Response::from_parts(
                200,
                std::collections::HashMap::from([(
                    "content-type".to_owned(),
                    "application/json".to_owned(),
                )]),
                body,
            )?;
            return Ok(ActorHttpResponse::Buffered(response));
        }
        Ok(ActorHttpResponse::Buffered(Response::from_parts(
            404,
            std::collections::HashMap::new(),
            Vec::new(),
        )?))
    }
}

/// Diagnostic action output; also the actor's minimal proof of life.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct InfoOutput {
    pub logical_id: String,
    pub actor_key: Vec<String>,
}

/// Reports the actor identity derived from the key.
///
/// Rivet actions carry their own arguments, so the message is empty and the
/// output is [`InfoOutput`].
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Info {}

impl rivetkit::Action for Info {
    type Output = InfoOutput;

    const NAME: &'static str = "info";
}

impl Handler<Info> for SwarmActor {
    async fn handle(self: Arc<Self>, _ctx: Ctx<Self>, _action: Info) -> Result<InfoOutput> {
        Ok(InfoOutput {
            logical_id: self.logical_id.as_str().to_owned(),
            actor_key: actor_key_segments(&self.logical_id),
        })
    }
}

/// `["agent", logicalId]` — the routing key rendered for diagnostics.
fn actor_key_segments(logical_id: &LogicalId) -> Vec<String> {
    ActorKey::new(logical_id.clone())
        .segments()
        .iter()
        .map(|segment| (*segment).to_owned())
        .collect()
}
