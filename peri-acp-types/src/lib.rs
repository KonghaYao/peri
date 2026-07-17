//! Pure DTO crate for TUI <-> ACP contract. Depends only on serde.

pub mod event_data;
pub mod hook;
pub mod interaction;
pub mod interaction_types;
pub mod mcp_types;
pub mod message;
pub mod peri_caps;
pub mod permission;
pub mod plugin_types;
pub use peri_caps::PeriCaps;
pub mod skill;
pub mod summary;
