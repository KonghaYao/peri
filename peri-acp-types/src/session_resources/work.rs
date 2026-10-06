use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::messages::MessageId;
use crate::session::MessagePolicy;
use crate::session_resources::{
    ControlAttempt, ControlState, SessionResourceError, SessionResourceErrorKind,
    SessionResourceResult,
};

#[path = "work/commands.rs"]
mod commands;
#[path = "work/effect.rs"]
mod effect;
#[path = "work/entities.rs"]
mod entities;
#[path = "work/mailbox.rs"]
mod mailbox;
#[path = "work/processing.rs"]
mod processing;
#[path = "work/query.rs"]
mod query;
#[path = "work/response.rs"]
mod response;
#[path = "work/transition.rs"]
mod transition;

pub use commands::*;
pub use entities::*;
pub use query::*;
pub use response::*;
pub use transition::*;

pub const DEFAULT_AGENT_MAX_ITERATIONS: usize = 500;
pub const MAX_WORK_PAGE_SIZE: u32 = 64;

pub(super) fn invalid(detail: &str) -> SessionResourceError {
    SessionResourceError::new(SessionResourceErrorKind::InvalidInput {
        detail: detail.into(),
    })
}

#[cfg(test)]
#[path = "work/domain_test.rs"]
mod tests;
