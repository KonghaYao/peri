//! Protocol-level types shared by the HTTP layer, the typed ports, and tests.

pub mod approval;
pub mod cursor;
pub mod error;
pub mod event;
pub mod identity;
pub mod input;
pub(crate) mod input_id;
pub mod item;
pub mod mcp;
pub mod official;
pub mod official_item;
pub mod session;
pub mod workspace;

pub use approval::{
    declared as declared_approvals, escalation_servers, required_action_json, requires_answer,
    ApprovalDecision, ApprovalPolicy, RequiredAction, ServerApproval, SettlementReason,
    APPROVAL_ID_PREFIX, MCP_APPROVAL_REQUEST, MCP_TOOL_CALL,
};
pub use cursor::{Cursor, EventCursor};
pub use error::{ErrorCode, ErrorKind, SwarmError};
pub use event::{StreamEvent, TurnEvent, TurnFailure};
pub use identity::{new_root_session_id, ActorKey, LogicalId};
pub use input::{
    ContentBlock, Disposition, InputKind, InputMode, InputOutcome, InputState, InputSubmission,
};
pub use item::{ItemDelta, ItemKind, ItemOrder, ItemPage, ItemSource, ItemView, ItemsWindow};
pub use mcp::{McpHeader, McpToolKind, McpToolSpec};
pub use official::{
    turn_id_for_item, OfficialContent, OfficialEvent, OfficialMessageItem, OfficialSession,
    OfficialSessionError, OfficialTurn, TurnPage, TurnStatus, TurnsWindow,
};
pub use session::{
    AgentView, CreateSessionResponse, EnvironmentKind, EnvironmentRef, PromptCacheOptions,
    PromptCacheTtl, SandboxState, SandboxView, SessionDeleted, SessionPage, SessionState,
    SessionStatus, SessionView, SessionsWindow, TurnView, MAX_METADATA_KEY_CHARS,
    MAX_METADATA_PAIRS, MAX_METADATA_VALUE_CHARS,
};
pub use workspace::resolve_cwd;
