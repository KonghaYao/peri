//! In-process ACP runtime and its projection into Swarm facts (S4).
//!
//! Module boundary (`design §1.2`): only [`runtime`] imports `peri-acp` /
//! `peri-acp-types` / `peri-resources`. [`projection`] is a pure JSON mapper,
//! [`reader`] owns append-only persistence plus broadcast, and [`service`]
//! implements [`crate::service::SwarmService`] on top of the two.
//!
//! [`swarm`] hosts one session per Rivet actor and exposes the typed action
//! plane plus the actor-side SSE endpoint; [`deployment`] is the process-level
//! install point for the store and service configuration.

pub mod deployment;
pub mod projection;
pub mod reader;
pub mod runtime;
pub mod service;
pub mod swarm;

pub use deployment::{install_deployment, ActorDeployment, StaticDeployment};
pub use reader::ItemLog;
pub use runtime::{AcpChild, CancelTarget, ChildConfig, ChildShutdown};
pub use service::{AcpSwarmService, ActorServiceConfig};
pub use swarm::{SwarmActor, ACTOR_NAME};
