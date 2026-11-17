//! ratatui-kit WorkflowPanel kanban component.
//!
//! Displays workflow run status in a kanban-style layout: run tabs at top,
//! phases on left, agents on right, footer shortcuts at bottom.

#[cfg(test)]
#[path = "workflow_test.rs"]
mod tests;

#[cfg(test)]
use crate::app::panel_types::PanelKind;
#[cfg(test)]
use crate::application::operations::CompletionGate;
#[cfg(test)]
use crate::i18n;
#[cfg(test)]
use std::time::Duration;

#[cfg(test)]
use crate::kit::workflow_snapshot::TuiRunProgress;
#[cfg(test)]
use ratatui_kit::crossterm::event::Event;
#[cfg(test)]
use ratatui_kit::crossterm::event::KeyCode;
#[cfg(test)]
use unicode_width::UnicodeWidthStr;

mod workflow_panel;
pub use workflow_panel::*;

mod agent_table_columns;
use self::agent_table_columns as table_layout;
#[cfg(test)]
#[cfg(test)]
use table_layout::{AgentTableColumns, agent_table_header};

#[cfg(test)]
use crate::kit::atoms::ACTIVE_SESSION_ID;

#[cfg(test)]
use crate::kit::atoms::BRIDGE_RESET_COUNTER;

#[cfg(test)]
use crate::kit::atoms::NOTIFICATION;

#[cfg(test)]
use crate::kit::atoms::Notification;

#[cfg(test)]
use crate::kit::atoms::SELECTED_WORKFLOW_RUN_ID;

#[cfg(test)]
use crate::kit::atoms::WORKFLOW_SNAPSHOT;
