//! Read-only snapshot injected into a panel before each event / render.
//!
//! Panels never hold mutable references to session state. They receive a
//! `PanelReadContext` snapshot and produce a list of `PanelEffect` instructions
//! that the state machine maps to standard `Effect`s.

use std::collections::HashMap;

use ratatui::layout::Rect;

use peri_acp_types::view_model::ViewModel;

use crate::i18n::LcRegistry;

// ---------------------------------------------------------------------------
// ServiceRegistrySnapshot
// ---------------------------------------------------------------------------

/// A lightweight, read-only subset of `ServiceRegistry` for panel injection.
///
/// Intentionally empty during P3 infrastructure scaffolding. Fields will be
/// added incrementally as each panel migration requires them (e.g. MCP server
/// list, cron tasks, config snapshot).
#[derive(Debug, Clone)]
pub struct ServiceRegistrySnapshot {
    // Future fields (added per-panel as needed):
    // pub config: Arc<PeriConfig>,
    // pub mcp_servers: Vec<McpServerDto>,
    // pub cron_tasks: Vec<CronTaskDto>,
    // pub hooks: Vec<HookDto>,
}

impl ServiceRegistrySnapshot {
    /// Create an empty snapshot. All fields are placeholder stubs for P3.
    pub fn new() -> Self {
        Self {}
    }
}

impl Default for ServiceRegistrySnapshot {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// PanelReadContext
// ---------------------------------------------------------------------------

/// Read-only snapshot injected into a panel before each key event / render.
///
/// Panels never hold mutable references to session state. They receive a
/// snapshot and produce a list of `PanelEffect` instructions that the state
/// machine maps to standard `Effect`s.
pub struct PanelReadContext<'a> {
    /// Read-only service registry snapshot.
    pub services: &'a ServiceRegistrySnapshot,
    /// Current ViewModel list (read-only).
    pub view_models: &'a [ViewModel],
    /// Current scroll offset in the message area.
    pub scroll_offset: u16,
    /// Panel area dimensions.
    pub area: Rect,
    /// i18n registry.
    pub lc: &'a LcRegistry,
    /// ACP query result cache (panel queries keyed by query name).
    pub acp_query_cache: &'a HashMap<String, serde_json::Value>,
}
