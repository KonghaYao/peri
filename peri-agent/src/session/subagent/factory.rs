use std::sync::Arc;

mod claim;
mod context;
mod resume;
mod spawn;

#[cfg(test)]
#[path = "factory/completion_delivery_test.rs"]
mod completion_delivery_tests;

use crate::session::subagent::types::{SubagentResumeConfig, SubagentSpawnConfig, SubagentSpawned};
use crate::session::Session;
pub(super) use claim::ResumeClaim;

use subagent_factory::completion_target;

use subagent_factory::persist_completion_target;

use subagent_factory::completion_delivery;

pub(super) use subagent_factory::validate_thread_id_format;

pub(super) use subagent_factory::execution_root;

pub use subagent_factory::SessionFactory;

#[path = "factory/subagent_factory.rs"]
mod subagent_factory;
