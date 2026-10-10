//! Tests for mod_agents_md

use peri_agent::middleware::r#trait::Middleware;

use super::*;

/// Helper: call prompt_contribution with concrete State type for testing.
fn contribution(mw: &AgentsMdMiddleware) -> Option<String> {
    Middleware::prompt_contribution(mw)
}

include!("agents_md_test.rs");
