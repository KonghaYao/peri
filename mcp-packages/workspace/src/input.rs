use std::fmt;
use std::fmt::{Debug, Formatter};
use std::sync::Arc;

use peri_acp_types::tasks::{OnBgCompleteFn, TaskManager};

#[path = "input/workspace_instance_input.rs"]
mod workspace_instance_input;
pub use workspace_instance_input::WorkspaceInstanceInput;
