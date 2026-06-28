//! Concrete interaction handlers (P3 will fill in the logic).
//!
//! Each handler implements [`crate::state_machine::state::Handler`] and wraps
//! the corresponding `peri-acp-types` payload. The P2 versions are
//! deserialisation-only stubs -- the state machine enters Modal with one of
//! these when the matching ACP event arrives.

pub mod ask_user;
pub mod hitl;
pub mod oauth;
pub mod rewind;

pub use ask_user::AskUserHandler;
pub use hitl::HitlHandler;
pub use oauth::OauthHandler;
pub use rewind::RewindHandler;
