mod acp_deployment;
pub mod client;
use self::acp_deployment as deployment;

/// 排队提交绑定的会话在 prompt 准入前已被切换：不得改投新会话。
pub(crate) const SESSION_BINDING_CHANGED_CODE: i64 = -32018;

/// 交互式 TUI 会话的专属指令，经 `session/new` 的 `_meta["peri.instructions"]` 冻结。
pub(crate) const TUI_SESSION_INSTRUCTIONS: &str = include_str!("../../prompts/tui.md");
pub use deployment::AcpDeployment;
pub mod interaction_lifecycle;
pub(crate) use interaction_lifecycle::SessionOperationId;
#[doc(hidden)]
pub mod interaction_response;
mod interaction_settlement;
pub use client::*;
pub use interaction_lifecycle::{
    InteractionExpiryReason, InteractionOwner, InteractionUiOutcome, ReverseInteractionKind,
};
