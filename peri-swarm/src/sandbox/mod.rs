//! Bun sandbox bridge client (`design §5.1`, `§5.3`).
//!
//! The Swarm gateway does not start SRT itself. A hosted environment is
//! *reserved* through the local Bun service: `POST /v1/sandboxes` with
//! `{"lazy":true}` allocates the sandbox identity, workspace path and outer MCP
//! URL without starting the sandbox. The first proxied MCP request starts it
//! inside Bun. The reservation response never carries the inner MCP token; this
//! client only keeps the deployment's outer bearer token.

mod client;

pub use client::{
    is_valid_logical_env_id, BunSandboxClient, SandboxClientConfig, SandboxClientError,
    SandboxMcpBinding, SandboxReservation,
};
