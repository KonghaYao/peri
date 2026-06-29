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
/// Populated once per render/key event from live `App` data. Panels that don't
/// store their own local state (e.g. status, mcp) can read from this snapshot.
#[derive(Debug, Clone)]
pub struct ServiceRegistrySnapshot {
    /// Working directory path.
    pub cwd: String,
    /// Active model alias ("opus" / "sonnet" / "haiku").
    pub model_alias: String,
    /// Active provider display name.
    pub provider_name: String,
    /// Active permission mode display string.
    pub permission_mode: String,
}

impl ServiceRegistrySnapshot {
    /// Create an empty snapshot with sensible defaults.
    pub fn new() -> Self {
        Self {
            cwd: String::new(),
            model_alias: String::new(),
            provider_name: String::new(),
            permission_mode: String::new(),
        }
    }

    /// Create a populated snapshot from live `App` data.
    pub fn from_app(app: &crate::app::App) -> Self {
        Self {
            cwd: app.services.cwd.clone(),
            model_alias: app.services.model_name.clone(),
            provider_name: app.services.provider_name.clone(),
            permission_mode: format!("{:?}", app.services.permission_mode.load()),
        }
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
