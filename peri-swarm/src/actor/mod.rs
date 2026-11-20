//! In-process ACP runtime and its projection into Swarm facts (S4).
//!
//! Module boundary (`design §1.2`): only [`runtime`] imports `peri-acp` /
//! `peri-acp-types` / `peri-resources`. [`projection`] is a pure JSON mapper,
//! [`reader`] owns append-only persistence plus broadcast, and [`service`]
//! implements [`crate::service::SwarmService`] on top of the two.
//!
//! This stage deliberately starts no Rivet actor and no HTTP listener; the
//! integration test drives [`service::AcpSwarmService`] directly.

pub mod projection;
pub mod reader;
pub mod runtime;
pub mod service;

pub use reader::ItemLog;
pub use runtime::{AcpChild, CancelTarget, ChildConfig, ChildShutdown};
pub use service::{AcpSwarmService, ActorServiceConfig};
