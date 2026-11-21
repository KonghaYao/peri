//! Protocol-level types shared by the HTTP layer, the typed ports, and tests.

pub mod cursor;
pub mod error;
pub mod event;
pub mod identity;
pub mod input;
pub mod item;
pub mod mcp;
pub mod session;
pub mod workspace;

pub use cursor::Cursor;
pub use error::{ErrorCode, ErrorKind, SwarmError};
pub use event::{StreamEvent, TurnEvent, TurnFailure};
pub use identity::{new_root_session_id, ActorKey, LogicalId};
pub use input::{
    ContentBlock, Disposition, InputKind, InputMode, InputOutcome, InputState, InputSubmission,
};
pub use item::{ItemKind, ItemOrder, ItemPage, ItemSource, ItemView, ItemsWindow};
pub use mcp::{McpHeader, McpToolKind, McpToolSpec};
pub use session::{
    AgentView, CreateSessionResponse, EnvironmentKind, EnvironmentRef, SandboxState, SandboxView,
    SessionState, SessionView, TurnView,
};
pub use workspace::resolve_cwd;
