//! Type contract layer between TUI and ACP. Depends only on serde.
//!
//! Three active modules remain after the 2026-07-20 dead-code audit:
//! - `event_data` — unstable-event payload structs consumed by peri-tui
//! - `peri_caps` — capability negotiation flags consumed by both peri-acp and peri-tui
//! - `summary` — migrated event DTOs re-exported via peri-acp::event

pub mod event_data;
pub mod peri_caps;
pub use peri_caps::PeriCaps;
pub mod summary;
